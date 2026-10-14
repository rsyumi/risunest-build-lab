export const sortableOptions = {
    delay: 300, // time in milliseconds to define when the sorting should start
    delayOnTouchOnly: true,
    filter: '.no-sort',
    onMove: (event) => {
        return event.related.className.indexOf('no-sort') === -1
    }
} as const
