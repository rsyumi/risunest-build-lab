import { mount } from 'svelte'
import SwitchContainment from './SwitchContainment.svelte'

const kind = new URLSearchParams(location.search).get('kind') === 'setting' ? 'setting' : 'switch'
mount(SwitchContainment, { target: document.querySelector('#lists')!, props: { kind } })
