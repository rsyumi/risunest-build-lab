import { getChatVar, getGlobalChatVar } from '../parser/chatVar.svelte';
import { calculateCbsExpression } from './cbsArithmetic';

export function calcString(text:string) {
    return calculateCbsExpression(text, { getChatVar, getGlobalChatVar })
}
