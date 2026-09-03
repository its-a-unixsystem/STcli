const MODULE = 'request-monitor-wire';
const DEFAULT_SETTINGS = Object.freeze({
    fetchUrl: 'https://fixture.invalid/fetch?source=monitor',
    ajaxUrl: 'https://fixture.invalid/ajax?source=monitor',
    enabled: true,
    requests: 0,
});
const STORE_KEY = 'request-monitor-wire-last';

function settings() {
    extension_settings[MODULE] ??= JSON.parse(JSON.stringify(DEFAULT_SETTINGS));
    for (const [key, value] of Object.entries(DEFAULT_SETTINGS)) {
        if (extension_settings[MODULE][key] === undefined) {
            extension_settings[MODULE][key] = value;
        }
    }
    return extension_settings[MODULE];
}

function remember(record) {
    localStorage.setItem(STORE_KEY, JSON.stringify(record));
    settings().requests += 1;
    saveSettingsDebounced();
}

async function requestMonitorFixtureInterceptor(chat) {
    const current = settings();
    if (!current.enabled) return chat;

    const fetchResponse = await fetch(current.fetchUrl, {
        method: 'POST',
        headers: {
            'content-type': 'application/json',
            'x-fixture-public': 'fetch',
        },
        body: JSON.stringify({ channel: 'fetch', input: 'fixture' }),
    });
    const fetchBody = await fetchResponse.json();

    let ajaxCallback = null;
    const ajaxBody = await $.ajax({
        url: current.ajaxUrl,
        type: 'POST',
        headers: {
            'content-type': 'application/json',
            'x-fixture-public': 'ajax',
        },
        data: JSON.stringify({ channel: 'ajax', input: 'fixture' }),
        dataType: 'json',
        success(data) {
            ajaxCallback = data;
        },
    });

    const record = {
        fetchStatus: fetchResponse.status,
        fetchBody,
        ajaxBody,
        ajaxCallback,
    };
    remember(record);
    chat.push({
        name: 'ST Request Monitor',
        is_user: false,
        is_system: true,
        mes: `wire:${fetchBody.result}:${ajaxBody.result}:${ajaxCallback.result}`,
        extra: {},
        index: chat.length,
    });
    return chat;
}

globalThis.requestMonitorFixtureInterceptor = requestMonitorFixtureInterceptor;

SillyTavern.registerSlashCommand('/wire-status', () => {
    const record = localStorage.getItem(STORE_KEY) || 'none';
    return `requests=${settings().requests};last=${record}`;
});
