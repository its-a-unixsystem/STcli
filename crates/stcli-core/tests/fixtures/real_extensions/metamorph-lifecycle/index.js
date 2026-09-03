const MODULE = 'metamorph-lifecycle';
const DEFAULT_SETTINGS = Object.freeze({
    enabled: true,
    turns: 0,
    providerProfile: 'summary',
});

let transientTurns = 0;

function settings() {
    extension_settings[MODULE] ??= JSON.parse(JSON.stringify(DEFAULT_SETTINGS));
    for (const [key, value] of Object.entries(DEFAULT_SETTINGS)) {
        if (extension_settings[MODULE][key] === undefined) {
            extension_settings[MODULE][key] = value;
        }
    }
    return extension_settings[MODULE];
}

function recordLifecycle(name) {
    settings().lastLifecycle = name;
    saveSettingsDebounced();
}

eventSource.on(event_types.APP_READY, () => recordLifecycle('app_ready'));
eventSource.on(event_types.CHAT_CHANGED, () => recordLifecycle('chat_changed'));
eventSource.on(event_types.MESSAGE_SENT, () => recordLifecycle('message_sent'));
eventSource.on(event_types.MESSAGE_RECEIVED, () => recordLifecycle('message_received'));
eventSource.on(event_types.GENERATION_ENDED, () => recordLifecycle('generation_ended'));

async function metamorphFixtureInterceptor(chat) {
    const current = settings();
    transientTurns += 1;
    current.turns += 1;
    saveSettingsDebounced();

    const quiet = await SillyTavern.generateQuietPrompt(
        'Describe the latest irreversible change.',
        { provider: current.providerProfile, temperature: 0 },
    );
    const raw = await SillyTavern.generateRaw(
        'Return the current transformation tier.',
        { providerProfile: current.providerProfile, temperature: 0 },
    );
    const marker = `metamorph:persistent=${current.turns}:transient=${transientTurns}:${quiet}:${raw}`;
    SillyTavern.setExtensionPrompt('metamorph.fixture', marker, 1, 1, false, 0);
    chat.push({
        name: 'Metamorph',
        is_user: false,
        is_system: true,
        mes: marker,
        extra: {},
        index: chat.length,
    });
    return chat;
}

globalThis.metamorphFixtureInterceptor = metamorphFixtureInterceptor;
globalThis.onActivate = async () => recordLifecycle('activate');
globalThis.onEnable = async () => recordLifecycle('enable');
globalThis.onDisable = async () => recordLifecycle('disable');
globalThis.onClean = async () => {
    delete extension_settings[MODULE];
    saveSettingsDebounced();
};

document.querySelector('#metamorph-fixture-panel');
$('#metamorph-fixture-panel').append('<span>Metamorph fixture</span>');
toastr.info('Metamorph fixture loaded');
