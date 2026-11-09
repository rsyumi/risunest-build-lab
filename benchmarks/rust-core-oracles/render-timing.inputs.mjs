// Synthetic chat messages for the display render timing (render-timing.test.ts and the Rust `wiring_cost` test). Each
// message repeats a block until it reaches its length in UTF-16 units, so the sizes share a shape.
// - `prose`: narration with dialogue, emphasis and a little Korean; the render asks the host only for the scripts.
// - `heavy`: headings, lists, a table, quotes, two `$$` spans and two fenced code blocks per block, asset tags and an
//   inlay, with one `<Thoughts>`, one `<style>` and one `{{source::char}}` per message.
// - `heavy-rewrite`: `heavy` with an `editdisplay` script that writes an asset tag, so the second asset pass runs too.

export const timingCharacter = {
    chaId: 'timing-character',
    assets: [
        ['Smile', 'assets/smile.png', 'png'], ['wall', 'assets/wall.webp', 'webp'], ['music', 'assets/music.mp3', 'mp3'],
        ['Clip', 'assets/clip.mp4', 'mp4'], ['portrait', 'assets/portrait.png', 'png'],
    ],
    emotions: [['happy', 'assets/emo-happy.png']],
}
export const timingInlays = ['img0', 'img1', 'img2'].map(id => ({ id, type: 'image', mime: 'image/png', width: 640, height: 480 }))
export const timingSizes = [1_000, 10_000, 30_000, 50_000, 100_000]
const rewrite = { in: 'SWAPME', out: '{{img::portrait}}' }

function prose(index) {
    return [
        `"You came back," Mira said, setting the lantern down on the table. *She did not look up.* Block ${index}.`,
        '',
        'The rain had not stopped since morning. Water ran along the gutters and pooled by the door, and the old clock above the hearth ticked on as if nothing had changed. "I said I would," he answered.',
        '',
        `밤새 내린 비에 길이 젖어 있었다. 그는 젖은 외투를 벗어 의자에 걸었다. "늦어서 미안해." *잠시 침묵이 흘렀다.* ${index}`,
        '',
        'She poured two cups of tea and pushed one across. Steam curled between them. "Sit. Tell me what you found, and do not leave anything out this time."',
        '',
    ].join('\n')
}

function heavy(index) {
    return [
        `## Scene ${index}`,
        '',
        `"Line ${index}," she said. *She smiles.* {{img::Smile}} and **bold** text with \`inline code\`.`,
        '',
        `- first item ${index}`,
        `- second item with [a link](https://example.com/${index})`,
        '',
        '| key | value |',
        '| --- | --- |',
        `| row | ${index} |`,
        '',
        `Energy $$E = mc^{2} + ${index}$$ and $$\\frac{a_{${index}}}{b} - x$$ -- done.`,
        '',
        '```js',
        `function scene${index}(x) { return x * ${index} + 1 }`,
        '```',
        '',
        '```python',
        `def scene_${index}(y):`,
        `    return y + ${index}`,
        '```',
        '',
        `> quoted line ${index}`,
        '',
        `{{raw::wall}} {{inlay::img${index % 3}}}`,
        '',
    ].join('\n')
}

function fill(block, length, head, tail) {
    let text = head
    for (let index = 0; text.length + tail.length < length; index++) text += block(index)
    return text + tail
}

export function timingInputs() {
    const inputs = []
    for (const size of timingSizes) {
        inputs.push({ id: `prose-${size}`, source: fill(prose, size, '', ''), scripts: [] })
        const head = '<Thoughts>Plan the scene first.</Thoughts>\n\n<style>.timing-box { margin: 0; color: #333; }</style>\n\nSWAPME\n\n'
        const source = fill(heavy, size, head, '{{source::char}}\n')
        inputs.push({ id: `heavy-${size}`, source, scripts: [] })
        inputs.push({ id: `heavy-rewrite-${size}`, source, scripts: [rewrite] })
    }
    return inputs
}
