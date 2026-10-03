export function filterPayloads(paths, query) {
  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean)
  if (!terms.length) return paths
  return paths.filter(path => {
    const name = path.split('/').pop().replace(/_/g, ' ').toLowerCase()
    return terms.every(term => name.includes(term))
  })
}
