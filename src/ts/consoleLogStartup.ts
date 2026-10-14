import { installAppConsoleCapture } from './consoleLog'

const dispose = installAppConsoleCapture()
if (import.meta.hot) import.meta.hot.dispose(dispose)
