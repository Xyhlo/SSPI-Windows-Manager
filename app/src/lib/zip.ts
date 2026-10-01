/* =====================================================================
   Small zip support for theme icon packs: writing stores files as they
   are (PNGs are already compressed), reading takes stored and deflated
   entries, so a folder zipped with Windows or any tool imports.
   ===================================================================== */

export type ZipEntry = { name: string; data: Uint8Array }

const TABLE = (() => {
  const table = new Uint32Array(256)
  for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; table[n] = c >>> 0 }
  return table
})()
export function crc32(data: Uint8Array) {
  let c = 0xffffffff
  for (let i = 0; i < data.length; i++) c = TABLE[(c ^ data[i]) & 255] ^ (c >>> 8)
  return (c ^ 0xffffffff) >>> 0
}

/** A zip of the entries, stored without compression. Names are UTF-8. */
export function zipStore(entries: ZipEntry[], when = new Date()): Uint8Array {
  const encoder = new TextEncoder()
  const time = (when.getHours() << 11) | (when.getMinutes() << 5) | (when.getSeconds() >> 1)
  const date = ((Math.max(1980, when.getFullYear()) - 1980) << 9) | ((when.getMonth() + 1) << 5) | when.getDate()
  const locals: Uint8Array[] = [], centrals: Uint8Array[] = []
  let offset = 0
  for (const entry of entries) {
    const name = encoder.encode(entry.name), crc = crc32(entry.data), size = entry.data.length
    const local = new Uint8Array(30 + name.length), lv = new DataView(local.buffer)
    lv.setUint32(0, 0x04034b50, true); lv.setUint16(4, 20, true); lv.setUint16(6, 0x0800, true); lv.setUint16(8, 0, true)
    lv.setUint16(10, time, true); lv.setUint16(12, date, true); lv.setUint32(14, crc, true); lv.setUint32(18, size, true); lv.setUint32(22, size, true)
    lv.setUint16(26, name.length, true); lv.setUint16(28, 0, true); local.set(name, 30)
    const central = new Uint8Array(46 + name.length), cv = new DataView(central.buffer)
    cv.setUint32(0, 0x02014b50, true); cv.setUint16(4, 20, true); cv.setUint16(6, 20, true); cv.setUint16(8, 0x0800, true); cv.setUint16(10, 0, true)
    cv.setUint16(12, time, true); cv.setUint16(14, date, true); cv.setUint32(16, crc, true); cv.setUint32(20, size, true); cv.setUint32(24, size, true)
    cv.setUint16(28, name.length, true); cv.setUint32(42, offset, true); central.set(name, 46)
    locals.push(local, entry.data); centrals.push(central)
    offset += local.length + size
  }
  const directory = centrals.reduce((n, c) => n + c.length, 0)
  const end = new Uint8Array(22), ev = new DataView(end.buffer)
  ev.setUint32(0, 0x06054b50, true); ev.setUint16(8, entries.length, true); ev.setUint16(10, entries.length, true)
  ev.setUint32(12, directory, true); ev.setUint32(16, offset, true)
  const out = new Uint8Array(offset + directory + 22)
  let at = 0
  for (const part of [...locals, ...centrals, end]) { out.set(part, at); at += part.length }
  return out
}

async function inflate(data: Uint8Array, expected: number) {
  const stream = new Blob([data as BlobPart]).stream().pipeThrough(new DecompressionStream("deflate-raw"))
  const out = new Uint8Array(await new Response(stream).arrayBuffer())
  if (out.length !== expected) throw new Error("A file in the zip is damaged.")
  return out
}

/** The files in a zip (folders skipped), up to `limit` bytes in total. */
export async function unzip(bytes: Uint8Array, limit = 64 << 20): Promise<ZipEntry[]> {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  let end = -1
  for (let i = bytes.length - 22; i >= Math.max(0, bytes.length - 22 - 65535); i--) if (view.getUint32(i, true) === 0x06054b50) { end = i; break }
  if (end < 0) throw new Error("That file isn't a zip archive.")
  const count = view.getUint16(end + 10, true)
  let at = view.getUint32(end + 16, true), total = 0
  const decoder = new TextDecoder()
  const entries: ZipEntry[] = []
  for (let n = 0; n < count; n++) {
    if (at + 46 > bytes.length || view.getUint32(at, true) !== 0x02014b50) throw new Error("The zip's file list is damaged.")
    const method = view.getUint16(at + 10, true), crc = view.getUint32(at + 16, true)
    const packed = view.getUint32(at + 20, true), size = view.getUint32(at + 24, true)
    const nameLength = view.getUint16(at + 28, true), extra = view.getUint16(at + 30, true), comment = view.getUint16(at + 32, true)
    const local = view.getUint32(at + 42, true)
    const name = decoder.decode(bytes.subarray(at + 46, at + 46 + nameLength))
    at += 46 + nameLength + extra + comment
    if (name.endsWith("/")) continue
    total += size
    if (total > limit) throw new Error("The zip holds more than 64 MiB.")
    if (local + 30 > bytes.length || view.getUint32(local, true) !== 0x04034b50) throw new Error("The zip is damaged.")
    const start = local + 30 + view.getUint16(local + 26, true) + view.getUint16(local + 28, true)
    const raw = bytes.subarray(start, start + packed)
    if (raw.length !== packed) throw new Error("The zip is cut short.")
    const data = method === 0 ? raw.slice() : method === 8 ? await inflate(raw, size) : null
    if (!data) throw new Error(`${name} uses a compression this app can't read.`)
    if (crc32(data) !== crc) throw new Error(`${name} is damaged.`)
    entries.push({ name, data })
  }
  return entries
}
