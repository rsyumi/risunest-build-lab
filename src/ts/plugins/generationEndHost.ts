import { v4 } from 'uuid'
import { subscribeGenerationEnd } from '../process/generationEnd'
import { getPersistentDataStore } from '../storage/persistentDataStoreFactory'
import { createGenerationEndEvents, createGenerationEndLocator } from './generationEndEvents'

export const generationEndEvents = createGenerationEndEvents({
    subscribe: subscribeGenerationEnd,
    locate: createGenerationEndLocator(() => ({
        store: getPersistentDataStore(),
        flushPendingData: async (reason) => {
            const { flushPendingDataLocally } = await import('../storage/persistentDataRuntime.svelte')
            await flushPendingDataLocally(reason)
        },
    })),
    createId: () => v4(),
})
