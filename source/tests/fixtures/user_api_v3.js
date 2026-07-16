/*!
 * @name lx-tui test source
 * @version 1.0.0
 */

const { EVENT_NAMES, on, send } = globalThis.lx;

on(EVENT_NAMES.request, ({ action, source, info }) => {
    if (action !== 'musicUrl') throw new Error(`unexpected action: ${action}`);
    if (source !== 'kw') throw new Error(`unexpected source: ${source}`);
    if (info.type !== '320k') throw new Error(`unexpected quality: ${info.type}`);
    if (info.musicInfo.songmid !== 'song-1') throw new Error('missing songmid');
    if (info.musicInfo.source !== 'kw') throw new Error('missing source');
    if (!info.musicInfo._types['320k']) throw new Error('missing quality metadata');
    return `https://example.com/${source}/${info.type}/${info.musicInfo.songmid}.mp3`;
});

send(EVENT_NAMES.inited, {
    sources: {
        kw: {
            type: 'music',
            actions: ['musicUrl'],
            qualitys: ['128k', '320k'],
        },
    },
});
