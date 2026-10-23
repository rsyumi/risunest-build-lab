import { defineConfig } from 'vite'
import { harnessConfig } from '../harnessConfig'
import { appLockMutationPlugin } from '../appLock/mutationPlugin'

const config = harnessConfig('ios')
export default defineConfig(async environment => {
    const resolved = await config(environment)
    resolved.plugins = [...(resolved.plugins ?? []), appLockMutationPlugin()]
    return resolved
})
