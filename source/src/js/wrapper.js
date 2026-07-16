// lx-tui compatibility runtime for the lx-music user API v3.
// Protocol messages use stdout; source-script logs are redirected to stderr.

const fs = require('fs');
const http = require('http');
const https = require('https');
const crypto = require('crypto');
const zlib = require('zlib');
const { Console } = require('console');
const { URL, URLSearchParams } = require('url');

const protocolWrite = (value) => {
    process.stdout.write(JSON.stringify(value) + '\n');
};

globalThis.console = new Console({
    stdout: process.stderr,
    stderr: process.stderr,
});
globalThis.window = globalThis;
globalThis.self = globalThis;

const sourcePath = process.argv[2];
if (!sourcePath) {
    protocolWrite({ type: 'initError', error: 'no source script path provided' });
    process.exit(1);
}

let sourceCode = '';
try {
    sourceCode = fs.readFileSync(sourcePath, 'utf8');
} catch (error) {
    protocolWrite({ type: 'initError', error: `read source failed: ${error.message}` });
    process.exit(1);
}

function metadataValue(name) {
    const match = sourceCode.match(new RegExp(`@${name}\\s+([^\\r\\n*]+)`));
    return match ? match[1].trim() : '';
}

const currentScriptInfo = {
    name: metadataValue('name') || 'lx-source',
    description: metadataValue('description'),
    version: metadataValue('version'),
    author: metadataValue('author'),
    homepage: metadataValue('homepage') || metadataValue('repository'),
    rawScript: sourceCode,
};

const EVENT_NAMES = {
    request: 'request',
    inited: 'inited',
    updateAlert: 'updateAlert',
};
let requestHandler = null;
let initialized = false;
let showedUpdateAlert = false;

function normalizeHeaders(headers) {
    const normalized = {};
    for (const [key, value] of Object.entries(headers || {})) {
        if (value != null) normalized[key] = String(value);
    }
    return normalized;
}

function encodeFormData(formData, headers) {
    const boundary = `----lx-tui-${crypto.randomBytes(12).toString('hex')}`;
    const parts = [];
    for (const [key, value] of Object.entries(formData || {})) {
        parts.push(Buffer.from(
            `--${boundary}\r\nContent-Disposition: form-data; name="${key}"\r\n\r\n`
        ));
        parts.push(Buffer.isBuffer(value) ? value : Buffer.from(String(value)));
        parts.push(Buffer.from('\r\n'));
    }
    parts.push(Buffer.from(`--${boundary}--\r\n`));
    headers['Content-Type'] = `multipart/form-data; boundary=${boundary}`;
    return Buffer.concat(parts);
}

function encodeRequestBody(options, headers) {
    if (options.body != null) {
        if (Buffer.isBuffer(options.body) || typeof options.body === 'string') {
            return options.body;
        }
        const contentType = Object.entries(headers)
            .find(([key]) => key.toLowerCase() === 'content-type')?.[1] || '';
        if (contentType.includes('application/x-www-form-urlencoded')) {
            return new URLSearchParams(options.body).toString();
        }
        if (!contentType) headers['Content-Type'] = 'application/json';
        return JSON.stringify(options.body);
    }
    if (options.form != null) {
        headers['Content-Type'] = 'application/x-www-form-urlencoded';
        return new URLSearchParams(options.form).toString();
    }
    if (options.formData != null) {
        return encodeFormData(options.formData, headers);
    }
    return null;
}

function decodeResponse(buffer, encoding, callback) {
    switch ((encoding || '').toLowerCase()) {
        case 'gzip':
            zlib.gunzip(buffer, callback);
            break;
        case 'deflate':
            zlib.inflate(buffer, callback);
            break;
        case 'br':
            zlib.brotliDecompress(buffer, callback);
            break;
        default:
            callback(null, buffer);
            break;
    }
}

function lxRequest(url, options = {}, callback) {
    let activeRequest = null;
    let cancelled = false;
    const maxRedirects = Number.isInteger(options.follow_max) ? options.follow_max : 5;

    const perform = (target, redirectsLeft) => {
        if (cancelled) return;

        let parsed;
        try {
            parsed = new URL(target);
        } catch (error) {
            callback(error, null, null);
            return;
        }

        const headers = normalizeHeaders(options.headers);
        const body = encodeRequestBody(options, headers);
        if (body != null && !Object.keys(headers).some((key) => key.toLowerCase() === 'content-length')) {
            headers['Content-Length'] = Buffer.byteLength(body);
        }

        const transport = parsed.protocol === 'https:' ? https : http;
        activeRequest = transport.request(parsed, {
            method: String(options.method || 'GET').toUpperCase(),
            headers,
        }, (response) => {
            const statusCode = response.statusCode || 0;
            const location = response.headers.location;
            if (location && statusCode >= 300 && statusCode < 400 && redirectsLeft > 0) {
                response.resume();
                perform(new URL(location, parsed).toString(), redirectsLeft - 1);
                return;
            }

            const chunks = [];
            response.on('data', (chunk) => chunks.push(Buffer.from(chunk)));
            response.on('end', () => {
                const raw = Buffer.concat(chunks);
                decodeResponse(raw, response.headers['content-encoding'], (error, decoded) => {
                    if (error) {
                        callback(error, null, null);
                        return;
                    }

                    const text = decoded.toString();
                    let parsedBody = text;
                    try {
                        parsedBody = JSON.parse(text);
                    } catch (_) {}

                    const result = {
                        statusCode,
                        statusMessage: response.statusMessage || '',
                        headers: response.headers,
                        bytes: decoded.length,
                        raw: decoded,
                        body: parsedBody,
                    };
                    callback(null, result, parsedBody);
                });
            });
        });

        const timeout = Math.min(
            Math.max(Number(options.timeout) || 60000, 1),
            60000,
        );
        activeRequest.setTimeout(timeout, () => {
            activeRequest.destroy(new Error('request timeout'));
        });
        activeRequest.on('error', (error) => {
            if (!cancelled) callback(error, null, null);
        });
        if (body != null) activeRequest.write(body);
        activeRequest.end();
    };

    perform(url, maxRedirects);
    return () => {
        cancelled = true;
        if (activeRequest && !activeRequest.destroyed) activeRequest.destroy();
        activeRequest = null;
    };
}

globalThis.lx = {
    EVENT_NAMES,
    request: lxRequest,
    send(eventName, data) {
        if (!Object.values(EVENT_NAMES).includes(eventName)) {
            return Promise.reject(new Error(`unsupported event: ${eventName}`));
        }
        if (eventName === EVENT_NAMES.inited) {
            if (initialized) return Promise.reject(new Error('script is inited'));
            initialized = true;
        } else if (eventName === EVENT_NAMES.updateAlert) {
            if (showedUpdateAlert) {
                return Promise.reject(new Error('update alert can only be sent once'));
            }
            showedUpdateAlert = true;
        }
        protocolWrite({ type: 'event', event: eventName, data });
        return Promise.resolve();
    },
    on(eventName, handler) {
        if (eventName !== EVENT_NAMES.request) {
            return Promise.reject(new Error(`unsupported event: ${eventName}`));
        }
        requestHandler = handler;
        return Promise.resolve();
    },
    utils: {
        crypto: {
            aesEncrypt(buffer, mode, key, iv) {
                const cipher = crypto.createCipheriv(mode, key, iv);
                return Buffer.concat([cipher.update(buffer), cipher.final()]);
            },
            rsaEncrypt(buffer, key) {
                const padded = Buffer.concat([Buffer.alloc(128 - buffer.length), buffer]);
                return crypto.publicEncrypt({
                    key,
                    padding: crypto.constants.RSA_NO_PADDING,
                }, padded);
            },
            randomBytes(size) {
                return crypto.randomBytes(size);
            },
            md5(value) {
                return crypto.createHash('md5').update(value).digest('hex');
            },
        },
        buffer: {
            from(...args) {
                return Buffer.from(...args);
            },
            bufToString(buffer, format) {
                return Buffer.from(buffer, 'binary').toString(format);
            },
        },
        zlib: {
            inflate(buffer) {
                return new Promise((resolve, reject) => {
                    zlib.inflate(buffer, (error, data) => {
                        if (error) reject(error);
                        else resolve(data);
                    });
                });
            },
            deflate(data) {
                return new Promise((resolve, reject) => {
                    zlib.deflate(data, (error, buffer) => {
                        if (error) reject(error);
                        else resolve(buffer);
                    });
                });
            },
        },
    },
    currentScriptInfo,
    version: '2.0.0',
    env: 'desktop',
};

function sendResult(id, result) {
    protocolWrite({ id, result });
}

function sendError(id, error) {
    protocolWrite({ id, error: error?.message || String(error) });
}

function handleCall(command) {
    if (!requestHandler) {
        sendError(command.id, 'no request handler registered');
        return;
    }

    const timeoutMs = 15000;
    let timer;
    const request = Promise.resolve().then(() => requestHandler({
        action: command.action,
        source: command.source,
        info: command.info || {},
    }));
    const timeout = new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`音源处理超时（${timeoutMs / 1000} 秒）`)), timeoutMs);
    });

    Promise.race([request, timeout]).then(
        (value) => sendResult(command.id, value),
        (error) => sendError(command.id, error),
    ).finally(() => clearTimeout(timer));
}

let inputBuffer = '';
process.stdin.on('data', (data) => {
    inputBuffer += data.toString();
    const lines = inputBuffer.split('\n');
    inputBuffer = lines.pop();
    for (const line of lines) {
        if (!line.trim()) continue;
        try {
            const command = JSON.parse(line);
            if (command.type === 'call') {
                handleCall(command);
            } else if (command.type === 'ping') {
                protocolWrite({ type: 'pong', id: command.id });
            }
        } catch (error) {
            console.error(error);
        }
    }
});
process.stdin.on('end', () => process.exit(0));

const initTimer = setTimeout(() => {
    if (!initialized) {
        protocolWrite({ type: 'initError', error: 'source initialization timed out' });
    }
}, 15000);

process.on('uncaughtException', (error) => {
    if (!initialized) protocolWrite({ type: 'initError', error: error.message });
    else console.error(error);
});
process.on('unhandledRejection', (error) => {
    const message = error?.message || String(error);
    if (!initialized) protocolWrite({ type: 'initError', error: message });
    else console.error(error);
});

try {
    require(sourcePath);
} catch (error) {
    protocolWrite({ type: 'initError', error: `load source failed: ${error.message}` });
}

if (initialized) clearTimeout(initTimer);
