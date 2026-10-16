import { getPluginDeviceKeyspace, type PluginDeviceKeyspace } from "./pluginDeviceKeyspace";

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

/** The same device keyspace, holding JSON values under their own space. */
export class SafeLocalPluginStorage {
    __classType = 'REMOTE_REQUIRED' as const;
    readonly #keyspace: PluginDeviceKeyspace;

    constructor(owner: string, keyspace = getPluginDeviceKeyspace(owner)) {
        this.#keyspace = keyspace;
    }

    async getItem<T>(key: string): Promise<T | null> {
        const stored = await this.#keyspace.getItem('json', key);
        if (stored === null) return null;
        try {
            return JSON.parse(stored) as T;
        } catch {
            return null;
        }
    }
    async setItem<T>(key: string, value: T): Promise<void> {
        // Avoid V8's fast-stringifier Zone allocation limit for large flat strings.
        const serialized = JSON.stringify(value, null);
        if (typeof serialized !== 'string') {
            throw new TypeError('getLocalPluginStorage values must be JSON-serializable');
        }
        await this.#keyspace.setItem('json', key, serialized);
    }
    async removeItem(key: string): Promise<void> {
        await this.#keyspace.removeItem('json', key);
    }
    async keys(): Promise<string[]> {
        return await this.#keyspace.keys('json');
    }
    async clear(): Promise<void> {
        await this.#keyspace.clear('json');
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
