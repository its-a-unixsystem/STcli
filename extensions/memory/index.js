// Headless compatibility port of SillyTavern/SillyTavern
// public/scripts/extensions/memory/index.js at
// 51ad27fb86d39a3daca3adaa970375c9670c12df.

const EXTENSION_ID = 'memory';
const PROMPT_KEY = '1_memory';
const TOKEN_ALLOWANCE = 64;
const DEFAULT_PROMPT = 'Ignore previous instructions. Summarize the most important facts and events in the story so far. If a summary already exists in your memory, use that as a base and expand with new facts. Limit the summary to {{words}} words or less. Your response should include nothing but the summary.';
const DEFAULTS = Object.freeze({
    memoryFrozen: false,
    source: 'main',
    prompt: DEFAULT_PROMPT,
    template: '[Summary: {{summary}}]',
    position: 0,
    role: 0,
    depth: 2,
    promptWords: 200,
    promptInterval: 10,
    promptForceWords: 0,
    overrideResponseLength: 0,
    maxMessagesPerRequest: 0,
    providerProfile: '',
    checkpoints: [],
});

function settings() {
    extension_settings[EXTENSION_ID] ??= {};
    const current = extension_settings[EXTENSION_ID];
    let changed = false;
    for (const [key, value] of Object.entries(DEFAULTS)) {
        if (current[key] === undefined) {
            current[key] = Array.isArray(value) ? [] : value;
            changed = true;
        }
    }
    if (!Array.isArray(current.checkpoints)) {
        current.checkpoints = [];
        changed = true;
    }
    if (changed) saveSettingsDebounced();
    return current;
}

function utf8Bytes(text) {
    const bytes = [];
    for (let index = 0; index < text.length; index++) {
        let point = text.charCodeAt(index);
        if (point >= 0xD800 && point <= 0xDBFF && index + 1 < text.length) {
            const low = text.charCodeAt(index + 1);
            if (low >= 0xDC00 && low <= 0xDFFF) {
                point = 0x10000 + ((point - 0xD800) << 10) + (low - 0xDC00);
                index++;
            }
        }
        if (point < 0x80) bytes.push(point);
        else if (point < 0x800) bytes.push(0xC0 | (point >> 6), 0x80 | (point & 0x3F));
        else if (point < 0x10000) bytes.push(0xE0 | (point >> 12), 0x80 | ((point >> 6) & 0x3F), 0x80 | (point & 0x3F));
        else bytes.push(0xF0 | (point >> 18), 0x80 | ((point >> 12) & 0x3F), 0x80 | ((point >> 6) & 0x3F), 0x80 | (point & 0x3F));
    }
    return bytes;
}

function sha256(text) {
    const rightRotate = (value, amount) => (value >>> amount) | (value << (32 - amount));
    const constants = [];
    const initial = [];
    let candidate = 2;
    while (constants.length < 64) {
        let prime = true;
        for (let divisor = 2; divisor * divisor <= candidate; divisor++) {
            if (candidate % divisor === 0) { prime = false; break; }
        }
        if (prime) {
            if (initial.length < 8) initial.push((Math.sqrt(candidate) * 0x100000000) | 0);
            constants.push((Math.pow(candidate, 1 / 3) * 0x100000000) | 0);
        }
        candidate++;
    }
    const bytes = utf8Bytes(text);
    const bitLength = bytes.length * 8;
    bytes.push(0x80);
    while ((bytes.length % 64) !== 56) bytes.push(0);
    const high = Math.floor(bitLength / 0x100000000);
    const low = bitLength >>> 0;
    for (let shift = 24; shift >= 0; shift -= 8) bytes.push((high >>> shift) & 0xFF);
    for (let shift = 24; shift >= 0; shift -= 8) bytes.push((low >>> shift) & 0xFF);
    const hash = initial.slice();
    const words = new Array(64);
    for (let offset = 0; offset < bytes.length; offset += 64) {
        for (let index = 0; index < 16; index++) {
            const base = offset + index * 4;
            words[index] = ((bytes[base] << 24) | (bytes[base + 1] << 16) | (bytes[base + 2] << 8) | bytes[base + 3]) | 0;
        }
        for (let index = 16; index < 64; index++) {
            const x = words[index - 15];
            const y = words[index - 2];
            const s0 = rightRotate(x, 7) ^ rightRotate(x, 18) ^ (x >>> 3);
            const s1 = rightRotate(y, 17) ^ rightRotate(y, 19) ^ (y >>> 10);
            words[index] = (words[index - 16] + s0 + words[index - 7] + s1) | 0;
        }
        let [a, b, c, d, e, f, g, h] = hash;
        for (let index = 0; index < 64; index++) {
            const s1 = rightRotate(e, 6) ^ rightRotate(e, 11) ^ rightRotate(e, 25);
            const choose = (e & f) ^ (~e & g);
            const temporary1 = (h + s1 + choose + constants[index] + words[index]) | 0;
            const s0 = rightRotate(a, 2) ^ rightRotate(a, 13) ^ rightRotate(a, 22);
            const majority = (a & b) ^ (a & c) ^ (b & c);
            const temporary2 = (s0 + majority) | 0;
            h = g; g = f; f = e; e = (d + temporary1) | 0;
            d = c; c = b; b = a; a = (temporary1 + temporary2) | 0;
        }
        hash[0] = (hash[0] + a) | 0; hash[1] = (hash[1] + b) | 0;
        hash[2] = (hash[2] + c) | 0; hash[3] = (hash[3] + d) | 0;
        hash[4] = (hash[4] + e) | 0; hash[5] = (hash[5] + f) | 0;
        hash[6] = (hash[6] + g) | 0; hash[7] = (hash[7] + h) | 0;
    }
    return hash.map(value => (value >>> 0).toString(16).padStart(8, '0')).join('');
}

function activeChat(context) {
    if (!context || !Array.isArray(context.chat)) return [];
    return context.chat.filter(message => message && !message.is_system && String(message.mes ?? message.content ?? '').length > 0);
}

function formatEntry(message, context) {
    const speaker = message.name || (message.is_user || message.role === 'user' ? context.name1 : context.name2);
    return `${speaker}:\n${String(message.mes ?? message.content ?? '')}`;
}

function canonicalPrefix(chat, context, cursor) {
    return chat.slice(0, cursor).map(message => formatEntry(message, context)).join('\n\n');
}

function validCheckpoint(context, current) {
    const chat = activeChat(context);
    for (let index = current.checkpoints.length - 1; index >= 0; index--) {
        const checkpoint = current.checkpoints[index];
        const cursor = Number(checkpoint?.dialogue_cursor);
        if (!Number.isInteger(cursor) || cursor < 0 || cursor > chat.length) continue;
        if (sha256(canonicalPrefix(chat, context, cursor)) === checkpoint.history_prefix_digest) {
            return checkpoint;
        }
    }
    return null;
}

function expand(text, values) {
    return String(text || '').replace(/\{\{\s*([^{}]+?)\s*\}\}/g, (match, name) =>
        Object.prototype.hasOwnProperty.call(values, name) ? String(values[name]) : match);
}

function renderSummary(context) {
    const current = settings();
    const checkpoint = validCheckpoint(context, current);
    const raw = checkpoint ? String(checkpoint.raw_summary || '') : '';
    SillyTavern.registerMacro('summary', raw);
    const rendered = raw ? expand(current.template, { summary: raw }) : '';
    SillyTavern.setExtensionPrompt(PROMPT_KEY, rendered, Number(current.position), Number(current.depth), false, Number(current.role));
    return checkpoint;
}

function wordCount(chat, start) {
    return chat.slice(start).reduce((count, message) => {
        const text = String(message.mes ?? message.content ?? '').trim();
        return count + (text ? text.split(/\s+/).length : 0);
    }, 0);
}

function selectRequest(context, current, checkpoint) {
    const chat = activeChat(context);
    const start = checkpoint ? Number(checkpoint.dialogue_cursor) : 0;
    const eligible = chat.slice(start, Math.max(start, chat.length - 1));
    if (!eligible.length) return null;
    const systemPrompt = expand(current.prompt, { words: current.promptWords }).trim();
    if (!systemPrompt) return null;
    const responseReserve = Number(current.overrideResponseLength) > 0
        ? Number(current.overrideResponseLength)
        : Number(context.generationSettings?.max_tokens || 0);
    const tokenLimit = Math.max(0, Number(context.generationSettings?.max_context || 0) - responseReserve);
    const previous = checkpoint ? String(checkpoint.raw_summary || '').trim() : '';
    const selected = [];
    const cap = Math.max(0, Number(current.maxMessagesPerRequest) || 0);
    for (const message of eligible) {
        const entry = formatEntry(message, context);
        const candidate = [previous, ...selected, entry].filter(Boolean).join('\n\n');
        if (SillyTavern.getTokenCount([systemPrompt, candidate].filter(Boolean).join('\n\n')) + TOKEN_ALLOWANCE > tokenLimit) break;
        selected.push(entry);
        if (cap > 0 && selected.length >= cap) break;
    }
    if (!selected.length) return null;
    const cursor = start + selected.length;
    return {
        systemPrompt,
        userPrompt: [previous, ...selected].filter(Boolean).join('\n\n'),
        cursor,
        digest: sha256(canonicalPrefix(chat, context, cursor)),
        branchId: String(context.chatId || ''),
    };
}

function shouldRefresh(context, current, checkpoint, force) {
    if (force) return true;
    if (current.memoryFrozen) return false;
    const chat = activeChat(context);
    const start = checkpoint ? Number(checkpoint.dialogue_cursor) : 0;
    const eligibleEnd = Math.max(start, chat.length - 1);
    const entries = eligibleEnd - start;
    const interval = Number(current.promptInterval) || 0;
    const words = Number(current.promptForceWords) || 0;
    return (interval > 0 && entries >= interval)
        || (words > 0 && wordCount(chat.slice(0, eligibleEnd), start) >= words);
}

async function refresh(force) {
    const current = settings();
    const before = SillyTavern.getContext();
    const checkpoint = validCheckpoint(before, current);
    if (!shouldRefresh(before, current, checkpoint, force)) return '';
    const request = selectRequest(before, current, checkpoint);
    if (!request) return '';
    const output = String(await SillyTavern.generateRaw({
        prompt: request.userPrompt,
        systemPrompt: request.systemPrompt,
        responseLength: Number(current.overrideResponseLength) || 0,
        providerProfile: String(current.providerProfile || ''),
    }) || '').trim();
    if (!output) return '';
    const receipt = SillyTavern.getLastInferenceReceipt();
    const after = SillyTavern.getContext();
    const afterChat = activeChat(after);
    if (String(after.chatId || '') !== request.branchId
        || request.cursor > afterChat.length
        || sha256(canonicalPrefix(afterChat, after, request.cursor)) !== request.digest
        || !receipt
        || receipt.status !== 'completed'
        || !receipt.attempt_id) return '';
    current.checkpoints.push(Object.freeze({
        branch_id: request.branchId,
        raw_summary: output,
        dialogue_cursor: request.cursor,
        history_prefix_digest: request.digest,
        attempt_id: receipt.attempt_id,
    }));
    saveSettingsDebounced();
    return output;
}

async function memoryGenerateInterceptor() {
    await refresh(false);
}

globalThis.memoryGenerateInterceptor = memoryGenerateInterceptor;

eventSource.on('pre_prompt', () => renderSummary(SillyTavern.getContext()));
eventSource.on(event_types.CHAT_COMPLETION_PROMPT_READY, () => {
    const context = SillyTavern.getContext();
    const current = settings();
    const checkpoint = validCheckpoint(context, current);
    const raw = checkpoint ? String(checkpoint.raw_summary || '') : '';
    SillyTavern.setExtensionPrompt(PROMPT_KEY, raw ? expand(current.template, { summary: raw }) : '', Number(current.position), Number(current.depth), false, Number(current.role));
});

SillyTavern.registerSlashCommand('/summarize', async (_named, unnamed) => {
    const text = String(unnamed || '').trim();
    if (!text) return refresh(true);
    const current = settings();
    const systemPrompt = expand(current.prompt, { words: current.promptWords }).trim();
    if (!systemPrompt) return '';
    return String(await SillyTavern.generateRaw({
        prompt: text,
        systemPrompt,
        responseLength: Number(current.overrideResponseLength) || 0,
        providerProfile: String(current.providerProfile || ''),
    }) || '').trim();
});

settings();
