import rfdc from 'rfdc'

const rfdcClone = rfdc({
  circles:false,
})

/**
 * structuredClone that falls back to rfdc (Really Fast Deep Clone) when structuredClone throws.
 */
export function safeStructuredClone<T>(data:T):T{
  try {
      return structuredClone(data)
  } catch (error) {
      return rfdcClone(data)
  }
}
