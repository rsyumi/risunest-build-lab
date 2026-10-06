export * from 'monaco-editor/editor'
import 'monaco-editor/features/register.all'
// register.all omits these editor contributions that the full entry still loads.
import 'monaco-editor/editor/contrib/caretOperations/browser/caretOperations'
import 'monaco-editor/editor/contrib/dropOrPasteInto/browser/copyPasteContribution'
import 'monaco-editor/editor/contrib/semanticTokens/browser/documentSemanticTokens'
import 'monaco-editor/languages/definitions/markdown/register'
import 'monaco-editor/languages/definitions/lua/register'
