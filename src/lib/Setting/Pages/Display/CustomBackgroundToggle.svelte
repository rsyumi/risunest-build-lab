<script lang="ts">
    import { imageAliases } from "src/ts/media/imageFormats"
    import { language } from 'src/lang';
    import { saveImage } from 'src/ts/storage/database.svelte';
    import { DBState } from 'src/ts/stores.svelte';
    import { selectSingleFile } from 'src/ts/util';
    import Check from 'src/lib/UI/GUI/CheckInput.svelte';
    let selecting = $state(false);
</script>

<div class="flex items-center mt-2">
    <Check
        check={selecting || DBState.db.customBackground !== ''}
        onChange={async (check) => {
            if (selecting) return;
            if (check) {
                selecting = true;
                try {
                    const d = await selectSingleFile(imageAliases, { image: true });
                    if (!d) return;
                    const img = await saveImage(d.data);
                    DBState.db.customBackground = img;
                } finally {
                    selecting = false;
                }
            } else {
                DBState.db.customBackground = '';
            }
        }}
        name={language.useCustomBackground}
    />
</div>
