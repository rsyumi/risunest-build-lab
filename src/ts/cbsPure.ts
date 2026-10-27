export function parseArray(p1:string): unknown[]{
    try {
        const arr = JSON.parse(p1)
        if(Array.isArray(arr)){
            return arr
        }
        return p1.split('§')
    } catch (error) {
        return p1.split('§')
    }
}

export function makeArray(p1: unknown[]): string{
    return JSON.stringify(p1.map((f) => {
        if(typeof(f) === 'string'){
            return f.replace(/::/g, '\\u003A\\u003A')
        }
        return f
    }))
}

export function createPureCbsCallbacks({ parseArray, makeArray, calcString }: {
    parseArray: (value: string) => unknown[]
    makeArray: (value: unknown[]) => string
    calcString: (value: string) => number
}) {
    return {
        makearray: (args: string[]): string => {
            return makeArray(args)
        },
        arrayshift: (args: string[]): string => {
            const arr = parseArray(args[0])
            arr.shift()
            return makeArray(arr)
        },
        arraypop: (args: string[]): string => {
            const arr = parseArray(args[0])
            arr.pop()
            return makeArray(arr)
        },
        arraypush: (args: string[]): string => {
            const arr = parseArray(args[0])
            arr.push(args[1])
            return makeArray(arr)
        },
        arraysplice: (args: string[]): string => {
            const arr = parseArray(args[0])
            arr.splice(Number(args[1]), Number(args[2]), args[3])
            return makeArray(arr)
        },
        button: (args: string[]): string => {
            return `<button class="button-default" risu-trigger="${args[1]}">${args[0]}</button>`
        },
        startswith: (args: string[]): string => {
            return args[0].startsWith(args[1]) ? '1' : '0'
        },
        endswith: (args: string[]): string => {
            return args[0].endsWith(args[1]) ? '1' : '0'
        },
        contains: (args: string[]): string => {
            return args[0].includes(args[1]) ? '1' : '0'
        },
        replace: (args: string[]): string => {
            return args[0].replaceAll(args[1], args[2])
        },
        split: (args: string[]): string => {
            return makeArray(args[0].split(args[1]))
        },
        join: (args: string[]): string => {
            return (parseArray(args[0])).join(args[1])
        },
        spread: (args: string[]): string => {
            return (parseArray(args[0])).join('::')
        },
        trim: (args: string[]): string => {
            return args[0].trim()
        },
        length: (args: string[]): string => {
            return args[0].length.toString()
        },
        arraylength: (args: string[]): string => {
            return parseArray(args[0]).length.toString()
        },
        lower: (args: string[]): string => {
            return args[0].toLocaleLowerCase()
        },
        upper: (args: string[]): string => {
            return args[0].toLocaleUpperCase()
        },
        capitalize: (args: string[]): string => {
            return args[0].charAt(0).toUpperCase() + args[0].slice(1)
        },
        round: (args: string[]): string => {
            return Math.round(Number(args[0])).toString()
        },
        floor: (args: string[]): string => {
            return Math.floor(Number(args[0])).toString()
        },
        ceil: (args: string[]): string => {
            return Math.ceil(Number(args[0])).toString()
        },
        abs: (args: string[]): string => {
            return Math.abs(Number(args[0])).toString()
        },
        remaind: (args: string[]): string => {
            return (Number(args[0]) % Number(args[1])).toString()
        },
        tonumber: (args: string[]): string => {
            return ([...args[0]].filter((v) => {
                return !isNaN(Number(v)) || v === '.'
            })).join('')
        },
        pow: (args: string[]): string => {
            return Math.pow(Number(args[0]), Number(args[1])).toString()
        },
        arrayelement: (args: string[]): string => {
            const element = parseArray(args[0]).at(Number(args[1])) ?? 'null'
            return typeof element === 'object' ? JSON.stringify(element) : String(element)
        },
        calc: (args: string[]): string => {
            return calcString(args[0]).toString()
        }
    }
}
