<script lang="ts">
    import { popupStore } from "src/ts/stores.svelte";
    import { sleep } from "src/ts/util";
    import { onDestroy, onMount } from "svelte";
    import { backNavigationLayer } from "src/ts/ui/modalNavigation";

    let styleString = $derived.by(() => {
        let styleString = '';
        const windowWidth = window.innerWidth;
        const windowHeight = window.innerHeight;
        const mouseX = popupStore.mouseX;
        const mouseY = popupStore.mouseY;

        if(mouseX < windowWidth / 2) {
            styleString += `left: ${mouseX}px;`;
        } else {
            styleString += `right: ${windowWidth - mouseX}px;`;
        }
        if(mouseY < windowHeight / 2) {
            styleString += `top: ${mouseY}px;`;
        } else {
            styleString += `bottom: ${windowHeight - mouseY}px;`;
        }
        return styleString;
    });

    const close = (() => {
        popupStore.children = null;
    });
    // Also forgets the opener, so its next tap opens the menu again instead of closing it.
    const closeOnBack = () => {
        popupStore.children = null;
        popupStore.openId = 0;
    };

    onMount(async () => {
        await sleep(0)
        document.addEventListener('click', close);
    })

    onDestroy(() => {
        document.removeEventListener('click', close);
    })

</script>

{#if popupStore.children}
    <div class="bg-darkbg border-darkborderc border rounded-md p-4 gap-2 flex flex-col fixed z-modal items-start" style={styleString} use:backNavigationLayer={{ close: closeOnBack }}>
        {@render popupStore.children()}
    </div>
{/if}