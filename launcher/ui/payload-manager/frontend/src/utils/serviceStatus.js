export function managerIsReady(status) {
  return !!status && status.edition === 'sspi-payload-manager' && status.protocol === 1 && status.ready === true
}

export function describeLoaderStatus(status) {
  if (!status || status.port !== 9021) return 'unknown'
  if (status.ready === true && status.state === 'listening' && status.scope === 'network' && status.evidence === 'listener-and-process') return 'listening'
  if (status.ready === false && status.state === 'unavailable' && status.evidence === 'listener-and-process') return 'unavailable'
  return 'unknown'
}
