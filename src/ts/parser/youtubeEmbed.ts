export const YOUTUBE_EMBED_PAGE_URL = 'https://update.rsyumi.workers.dev/embed/youtube/v1'

const pageOrigin = new URL(YOUTUBE_EMBED_PAGE_URL).origin

function parseEmbedUrl(value: string): URL | null {
    try {
        const url = new URL(value)
        if (url.protocol !== 'https:' || url.username || url.password || url.port
            || !['www.youtube.com', 'www.youtube-nocookie.com'].includes(url.hostname)
            || !url.pathname.startsWith('/embed/')) return null
        return url
    } catch {
        return null
    }
}

function unwrapEmbedUrl(value: string): URL | null {
    const direct = parseEmbedUrl(value)
    if (direct) return direct
    try {
        const page = new URL(value)
        if (page.username || page.password || `${page.origin}${page.pathname}${page.search}` !== YOUTUBE_EMBED_PAGE_URL) return null
        const params = new URLSearchParams(page.hash.slice(1))
        if ([...params.keys()].length !== 1 || !params.has('src')) return null
        return parseEmbedUrl(params.get('src')!)
    } catch {
        return null
    }
}

/** Adapt only approved embeds; the caller removes every other iframe. */
export function prepareYouTubeIframe(node: HTMLElement): boolean {
    const source = unwrapEmbedUrl(node.getAttribute('src') ?? '')
    if (!source) return false
    node.setAttribute('src', `${YOUTUBE_EMBED_PAGE_URL}#${new URLSearchParams({ src: source.href })}`)
    const allow = node.getAttribute('allow')
    if (allow) {
        // Explicit origins must follow the new immediate child, while 'none' and 'src' keep their meaning.
        node.setAttribute('allow', allow.split(';').map(directive => directive.trim().split(/\s+/).map(token => {
            try {
                return new URL(token).origin === source.origin ? pageOrigin : token
            } catch {
                return token
            }
        }).join(' ')).join('; '))
    }
    return true
}
