import { getPluginDeviceKeyspace, type PluginDeviceKeyspace } from "./pluginDeviceKeyspace";
import { isTauri } from "../platform";
import {
    STRUCTURE_PART_BYTES, decodeStructuredLocalPluginValue, encodeStructuredLocalPluginValue, isPlainLocalPluginValue,
    isStructureText, parseStructureText, structurePartCount, structureText,
} from "./localPluginStorageValue";
import { PLUGIN_DEVICE_VALUE_BYTES, pluginDeviceValueSize, readNativePluginDevicePart, storeNativePluginDeviceParts } from "./pluginDeviceTransfer";

/**
 * Device-local strings for one plugin. Every call is answered from that
 * plugin's keyspace alone, and a write settles only once the store has kept it.
 */
export class SafeLocalStorage {
    readonly #keyspace: PluginDeviceKeyspace;

    constructor(owner: string, keyspace = getPluginDeviceKeyspace(owner)) {
        this.#keyspace = keyspace;
    }

    async getItem(key: string): Promise<string | null> {
        return await this.#keyspace.getItem('string', key);
    }
    async setItem(key: string, value: string): Promise<void> {
        await this.#keyspace.setItem('string', key, value);
    }
    async removeItem(key: string): Promise<void> {
        await this.#keyspace.removeItem('string', key);
    }
    //not a standard localStorage method, but useful
    async keys(): Promise<string[]> {
        return await this.#keyspace.keys('string');
    }

    async key(index: number): Promise<string | null> {
        const safeKeys = await this.keys();
        return safeKeys[index] ?? null;
    }

    async clear(): Promise<void> {
        await this.#keyspace.clear('string');
    }

    async length(): Promise<number> {
        return (await this.keys()).length;
    }
}

type LocalQueue = { keys: Map<string, Promise<void>>; clear: Promise<void> | null; enumeration: Promise<void> | null };
const localQueues = new WeakMap<PluginDeviceKeyspace, LocalQueue>();
const placeholderPart = '0'.repeat(64);

/**
 * The same device keyspace, holding values under their own space. A value JSON
 * reproduces exactly is stored as JSON text; any other value in the structured
 * clone range is stored in structure form with its bytes as device parts.
 */
export class SafeLocalPluginStorage {
    __classType = 'REMOTE_REQUIRED' as const;
    readonly #owner: string;
    readonly #keyspace: PluginDeviceKeyspace;
    readonly #queueState: LocalQueue;

    constructor(owner: string, keyspace = getPluginDeviceKeyspace(owner)) {
        this.#owner = owner;
        this.#keyspace = keyspace;
        let queue = localQueues.get(keyspace);
        if (!queue) { queue = { keys: new Map(), clear: null, enumeration: null }; localQueues.set(keyspace, queue); }
        this.#queueState = queue;
    }

    async getItem<T>(key: string): Promise<T | null> {
        return this.#queue(key, async () => {
            const stored = await this.#keyspace.getItem('json', key);
            if (stored === null) return null;
            if (isStructureText(stored)) {
                // A value this build cannot read fails for its own key only.
                try {
                    const structure = parseStructureText(stored);
                    return await decodeStructuredLocalPluginValue<T>(structure, index => readNativePluginDevicePart(
                        structure.parts[index], Math.min(STRUCTURE_PART_BYTES, structure.byteLength - index * STRUCTURE_PART_BYTES)));
                } catch (error) {
                    throw new Error('getLocalPluginStorage cannot read this value', { cause: error });
                }
            }
            try {
                return JSON.parse(stored) as T;
            } catch {
                return null;
            }
        }, false);
    }
    async setItem<T>(key: string, value: T): Promise<void> {
        // Upstream's store keeps a top-level undefined as null.
        const stored = value === undefined ? null : value;
        if (isPlainLocalPluginValue(stored)) {
            // Avoid V8's fast-stringifier Zone allocation limit for large flat strings.
            const serialized = JSON.stringify(stored, null);
            await this.#queue(key, () => this.#keyspace.setItem('json', key, serialized));
            return;
        }
        // Encode before any await so later changes to the value are not stored.
        const encoded = encodeStructuredLocalPluginValue(stored);
        if (!isTauri) throw new TypeError('getLocalPluginStorage values that are not JSON are not supported on the web');
        const placeholders = new Array<string>(structurePartCount(encoded.byteLength)).fill(placeholderPart);
        pluginDeviceValueSize(structureText(encoded, placeholders), PLUGIN_DEVICE_VALUE_BYTES - encoded.byteLength);
        await this.#queue(key, async () => {
            const parts = await storeNativePluginDeviceParts(this.#owner, encoded);
            await this.#keyspace.setItem('json', key, structureText(encoded, parts));
        });
    }
    #queue<T>(key: string | null, operation: () => Promise<T>, mutation = true): Promise<T> {
        const state = this.#queueState;
        const barriers: Promise<void>[] = key === null ? [...state.keys.values()] : [];
        const pending = key === null ? undefined : state.keys.get(key);
        if (pending) barriers.push(pending);
        if (state.clear) barriers.push(state.clear);
        if ((mutation || key === null) && state.enumeration) barriers.push(state.enumeration);
        const next = barriers.length === 0 ? operation()
            : barriers.length === 1 ? barriers[0].then(operation) : Promise.all(barriers).then(operation);
        const release = () => {
            if (key !== null) { if (state.keys.get(key) === settled) state.keys.delete(key); }
            else if (mutation) { if (state.clear === settled) state.clear = null; }
            else if (state.enumeration === settled) state.enumeration = null;
        };
        const settled = next.then(release, release);
        if (key === null) {
            if (mutation) state.clear = settled;
            else state.enumeration = settled;
        } else {
            state.keys.set(key, settled);
        }
        return next;
    }
    async removeItem(key: string): Promise<void> {
        await this.#queue(key, () => this.#keyspace.removeItem('json', key));
    }
    async keys(): Promise<string[]> {
        return await this.#queue(null, () => this.#keyspace.keys('json'), false);
    }
    async clear(): Promise<void> {
        await this.#queue(null, () => this.#keyspace.clear('json'));
    }
}


export const tagWhitelist = [
    'a',
    'abbr',
    'acronym',
    'address',
    'area',
    'article',
    'aside',
    'audio',
    'b',
    'bdi',
    'bdo',
    'big',
    'blink',
    'blockquote',
    'body',
    'br',
    'button',
    'canvas',
    'caption',
    'center',
    'cite',
    'code',
    'col',
    'colgroup',
    'content',
    'data',
    'datalist',
    'dd',
    'decorator',
    'del',
    'details',
    'dfn',
    'dialog',
    'dir',
    'div',
    'dl',
    'dt',
    'element',
    'em',
    'fieldset',
    'figcaption',
    'figure',
    'font',
    'footer',
    'form',
    'h1',
    'h2',
    'h3',
    'h4',
    'h5',
    'h6',
    'head',
    'header',
    'hgroup',
    'hr',
    'html',
    'i',
    'img',
    'input',
    'ins',
    'kbd',
    'label',
    'legend',
    'li',
    'main',
    'map',
    'mark',
    'marquee',
    'menu',
    'menuitem',
    'meter',
    'nav',
    'nobr',
    'ol',
    'optgroup',
    'option',
    'output',
    'p',
    'picture',
    'pre',
    'progress',
    'q',
    'rp',
    'rt',
    'ruby',
    's',
    'samp',
    'search',
    'section',
    'select',
    'shadow',
    'slot',
    'small',
    'source',
    'spacer',
    'span',
    'strike',
    'strong',
    'style',
    'sub',
    'summary',
    'sup',
    'table',
    'tbody',
    'td',
    'template',
    'textarea',
    'tfoot',
    'th',
    'thead',
    'time',
    'tr',
    'track',
    'tt',
    'u',
    'ul',
    'var',
    'video',
    'wbr',
    'svg',
    'a',
    'altglyph',
    'altglyphdef',
    'altglyphitem',
    'animatecolor',
    'animatemotion',
    'animatetransform',
    'circle',
    'clippath',
    'defs',
    'desc',
    'ellipse',
    'enterkeyhint',
    'exportparts',
    'filter',
    'font',
    'g',
    'glyph',
    'glyphref',
    'hkern',
    'image',
    'inputmode',
    'line',
    'lineargradient',
    'marker',
    'mask',
    'metadata',
    'mpath',
    'part',
    'path',
    'pattern',
    'polygon',
    'polyline',
    'radialgradient',
    'rect',
    'stop',
    'style',
    'switch',
    'symbol',
    'text',
    'textpath',
    'title',
    'tref',
    'tspan',
    'view',
    'vkern',
];
