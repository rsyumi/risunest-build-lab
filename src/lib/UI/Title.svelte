<script lang="ts">
    import { DBState } from 'src/ts/stores.svelte';
    import { openURL } from "src/ts/globalApi.svelte";
    import { ColorSchemeTypeStore } from "src/ts/gui/colorscheme";
    import { anniversaryYears, getSpecialDay, ordinal } from "src/ts/ui/specialDay";

    const today = new Date()
    const specialDay = getSpecialDay(today, DBState.db.language)
    const years = anniversaryYears(today)

    // Nest mark from src-tauri/icon-src/logo.svg: back rim (lip) and front wall (bowl).
    const LIP = 'M 217.47 282.425 C 220.018 282.137 224.36 281.539 226.34 281.37 C 232.093 280.879 236.35 280.6 241.21 280.22 C 248.37 279.65 255.47 278.52 262.48 276.99 C 269.65 275.42 276.74 273.42 283.7 271.12 C 316.67 260.23 372.15 229.92 350.89 186.69 C 347.9 180.61 343.38 175.77 338.62 171.04 C 375.56 175.69 402.25 194.39 419.84 227.12 L 420.24 229.95 C 418.43 241.63 418.85 253.19 412.79 263.79 C 389.25 304.96 322.97 305.62 281.69 300.71 C 267.59 299.04 227.18 291.925 217.47 282.425 Z'
    const BOWL = 'M 249.5 126.77 C 237.36 130.41 225.03 133.02 213.33 138.1 C 172.93 155.66 125.66 204.78 165.14 247.43 C 171.82 254.65 179.86 260.52 188.34 265.46 C 199.16 271.76 211.081 276.383 222.92 280.92 C 266.34 297.37 319.61 305.05 362.65 292.59 C 388.71 285.05 414.01 266.12 415.1 236.82 C 415.38 229.17 413.72 221.49 411.85 214.11 C 414.7 218.17 417.36 222.51 419.84 227.12 C 441.74 267.85 440.65 324.06 409.9 359.98 C 399.44 372.2 385.88 381.21 371.4 388.01 C 351.73 397.25 330.49 402.47 309.04 405.43 C 295.15 407.35 281.21 408.54 267.2 408.93 C 194.52 410.96 94.22 399.38 75.24 314 C 72.24 300.5 71.49 286.52 72.49 272.74 C 73.77 255.13 77.88 237.69 84.57 221.36 C 95.93 193.67 115.26 170.18 140.31 153.76 C 174.48 131.35 209.4 125.56 249.5 126.77 Z'
    const RAY = 'M 0 -98 L 9 -62 L -9 -62 Z'

    let hatTilt = $state(-16)
    let clicks = $state(0)
    let score = $state(0)
    let time = $state(20)
    let miniGameStart = $state(false)
    let hatPosition = $state(0)

    const onHatClick = () => {
        hatTilt = -40 + Math.random() * 50
        clicks++
        if(clicks === 5){
            hatTilt = -16
        }
    }
</script>

{#snippet santaHat()}
    <path d="M -58 0 C -42 -52 -6 -84 34 -108 C 20 -62 44 -30 62 0 Z" fill="#E03131"></path>
    <path d="M -58 0 C -42 -52 -6 -84 34 -108 C 14 -70 10 -40 2 0 Z" fill="#C92A2A" opacity="0.55"></path>
    <rect x="-70" y="-10" width="140" height="24" rx="12" fill="#FFFFFF"></rect>
    <circle cx="36" cy="-110" r="15" fill="#FFFFFF"></circle>
{/snippet}

<h2 class="text-4xl text-textcolor mb-0 mt-6 font-black flex items-center gap-3">
    <svg class="h-[1.1em] w-[1.41em] overflow-visible shrink-0" class:rotate-180={specialDay === 'aprilFool'} viewBox="66 120 380 296" aria-hidden="true">
        <defs>
            <linearGradient id="risunest-title-lip" gradientUnits="userSpaceOnUse" x1="0" y1="234" x2="0" y2="296"><stop offset="0" stop-color="#CEE1FD"></stop><stop offset="1" stop-color="#B3CFFC"></stop></linearGradient>
            <linearGradient id="risunest-title-bowl" gradientUnits="userSpaceOnUse" x1="0" y1="290" x2="0" y2="392"><stop offset="0" stop-color="#FEFEFE"></stop><stop offset="1" stop-color="#DCEAFD"></stop></linearGradient>
            <linearGradient id="risunest-title-sun" gradientUnits="userSpaceOnUse" x1="0" y1="150" x2="0" y2="270"><stop offset="0" stop-color="#FDE68A"></stop><stop offset="1" stop-color="#F97316"></stop></linearGradient>
            <linearGradient id="risunest-title-moon" gradientUnits="userSpaceOnUse" x1="0" y1="40" x2="0" y2="200"><stop offset="0" stop-color="#FFF4C2"></stop><stop offset="1" stop-color="#F8D978"></stop></linearGradient>
        </defs>
        {#if specialDay === 'harvestMoon'}
            <circle cx="348" cy="118" r="92" fill="#FDE68A" opacity="0.16"></circle>
            <circle cx="348" cy="118" r="70" fill="url(#risunest-title-moon)"></circle>
            <circle cx="322" cy="96" r="12" fill="#EAC96A" opacity="0.5"></circle>
            <circle cx="372" cy="128" r="18" fill="#EAC96A" opacity="0.45"></circle>
            <circle cx="340" cy="150" r="9" fill="#EAC96A" opacity="0.5"></circle>
        {/if}
        {#if $ColorSchemeTypeStore === 'light'}
            <path d={LIP} fill="currentColor" opacity="0.55"></path>
        {:else}
            <path d={LIP} fill="url(#risunest-title-lip)"></path>
        {/if}
        {#if specialDay === 'newYear'}
            <g transform="translate(262 212)">
                <g fill="#FDBA74" opacity="0.9">
                    {#each [-90, -60, -30, 0, 30, 60, 90] as angle}
                        <path d={RAY} transform="rotate({angle})"></path>
                    {/each}
                </g>
                <circle r="56" fill="url(#risunest-title-sun)"></circle>
            </g>
        {:else if specialDay === 'halloween'}
            <g transform="translate(258 222)">
                <rect x="-10" y="-80" width="20" height="30" rx="5" fill="#6B4F2A"></rect>
                <ellipse rx="66" ry="54" fill="#F97316"></ellipse>
                <ellipse rx="40" ry="54" fill="#EA6A0C"></ellipse>
                <ellipse rx="16" ry="54" fill="#F97316"></ellipse>
                <path d="M -34 -16 L -14 -8 L -30 4 Z M 34 -16 L 14 -8 L 30 4 Z" fill="var(--risu-theme-bgcolor)"></path>
                <path d="M -40 16 L -26 26 L -14 16 L 0 28 L 14 16 L 26 26 L 40 16 L 30 34 L -30 34 Z" fill="var(--risu-theme-bgcolor)"></path>
            </g>
        {/if}
        {#if $ColorSchemeTypeStore === 'light'}
            <path d={BOWL} fill="currentColor" fill-rule="evenodd"></path>
        {:else}
            <path d={BOWL} fill="url(#risunest-title-bowl)" fill-rule="evenodd"></path>
        {/if}
        {#if specialDay === 'christmas' && clicks < 5}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <g class="cursor-pointer transition-transform" transform="translate(222 142) rotate({hatTilt})" onclick={onHatClick}>
                {@render santaHat()}
            </g>
        {:else if specialDay === 'anniversary'}
            <g transform="translate(226 140) rotate(-14)">
                <path d="M -42 0 L 0 -108 L 42 0 Z" fill="#5878FD"></path>
                <path d="M -28 -36 L 0 -108 L 28 -36 Z" fill="#05AAB5"></path>
                <path d="M -14 -72 L 0 -108 L 14 -72 Z" fill="#F59E0B"></path>
                <circle cx="0" cy="-110" r="12" fill="#F59E0B"></circle>
            </g>
            <rect x="300" y="100" width="12" height="20" rx="2" fill="#F59E0B" transform="rotate(20 306 110)"></rect>
            <rect x="350" y="130" width="12" height="20" rx="2" fill="#05AAB5" transform="rotate(-30 356 140)"></rect>
            <circle cx="385" cy="110" r="7" fill="#5878FD"></circle>
            <circle cx="330" cy="70" r="6" fill="#E03131"></circle>
            <rect x="150" y="90" width="10" height="18" rx="2" fill="#5878FD" transform="rotate(35 155 99)"></rect>
        {/if}
    </svg>
    <span>RisuNest</span>
    <span class="text-base font-normal text-textcolor2 self-end pb-[0.42rem] whitespace-nowrap">by. Yumi</span>
</h2>

{#if specialDay === 'anniversary'}
    <h1>
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <span class="text-base text-textcolor2 hover:text-textcolor cursor-pointer transition" role="button" tabindex="-1" onclick={() => {
            openURL('https://github.com/rsyumi/RisuNest')
        }}>Happy <span class="font-semibold">{ordinal(years)}</span> Anniversary!</span>
    </h1>
{/if}
{#if clicks >= 5}
    <div class="bg-black w-full p-3 mt-4 mb-4 rounded-md max-w-2xl" id="minigame-div">
        <span class="font-semibold text-lg">Score: {score}</span><br>
        <span class="font-semibold text-lg">Time: {time.toFixed(0)}</span>
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <svg class="h-14 w-14 cursor-pointer" viewBox="-75 -130 150 150" role="button" tabindex="-1" aria-label="santa hat"
            style:margin-left={hatPosition + 'px'}
            class:grayscale={!miniGameStart}
            onclick={async () => {
                const miniGameDiv = document.getElementById('minigame-div')
                const max = miniGameDiv.clientWidth - 70
                hatPosition = Math.random() * max
                if(!miniGameStart){
                    if(time === 0){
                        time = 20
                        hatPosition = 0
                        return
                    }
                    time = 20
                    score = 1
                    miniGameStart = true
                    const timer = setInterval(() => {
                        time -= 1
                        if(time <= 0){
                            miniGameStart = false
                            clearInterval(timer)
                        }
                    }, 700)
                }
                else{
                    score++
                }
            }}
        >
            {@render santaHat()}
        </svg>
    </div>
{/if}
