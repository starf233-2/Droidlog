/**
 * Generates the droidlog app icon set with zero external dependencies.
 *
 * Tauri's Windows build embeds `icons/icon.ico` as an executable resource, so real
 * icon files must exist before the first `cargo tauri dev`.
 *
 * The artwork follows the project's design rules: fully flat, no gradients, no
 * saturation, no glow. Just a desaturated gray-blue plate with lighter log bars.
 *
 * Usage: node scripts/gen-icons.mjs
 */
import { deflateSync } from 'node:zlib'
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const HERE = dirname(fileURLToPath(import.meta.url))
const ICON_DIR = join(HERE, '..', 'src-tauri', 'icons')

/** Desaturated gray-blue plate — matches --dl-primary in the M3 token sheet. */
const PLATE = [0x5b, 0x6e, 0x80]
/** Muted text bars — matches --dl-on-primary-container. */
const BAR = [0xd7, 0xdf, 0xe6]

/* ------------------------------------------------------------------ PNG ---- */

const CRC_TABLE = (() => {
  const table = new Uint32Array(256)
  for (let n = 0; n < 256; n += 1) {
    let c = n
    for (let k = 0; k < 8; k += 1) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    }
    table[n] = c >>> 0
  }
  return table
})()

/** @param {Buffer} buf @returns {number} */
function crc32(buf) {
  let c = 0xffffffff
  for (let i = 0; i < buf.length; i += 1) {
    c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8)
  }
  return (c ^ 0xffffffff) >>> 0
}

/**
 * @param {string} type 4 ASCII chars
 * @param {Buffer} data
 * @returns {Buffer}
 */
function chunk(type, data) {
  const len = Buffer.alloc(4)
  len.writeUInt32BE(data.length, 0)
  const typeBuf = Buffer.from(type, 'ascii')
  const crc = Buffer.alloc(4)
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])), 0)
  return Buffer.concat([len, typeBuf, data, crc])
}

/**
 * Encodes straight (non-premultiplied) RGBA8 pixels as a PNG buffer.
 * @param {Uint8Array} rgba length must be width * height * 4
 * @param {number} width
 * @param {number} height
 * @returns {Buffer}
 */
function encodePng(rgba, width, height) {
  const stride = width * 4
  // Each scanline is prefixed with filter type 0 (None).
  const raw = Buffer.alloc((stride + 1) * height)
  for (let y = 0; y < height; y += 1) {
    raw[y * (stride + 1)] = 0
    Buffer.from(rgba.buffer, rgba.byteOffset + y * stride, stride).copy(
      raw,
      y * (stride + 1) + 1,
    )
  }

  const ihdr = Buffer.alloc(13)
  ihdr.writeUInt32BE(width, 0)
  ihdr.writeUInt32BE(height, 4)
  ihdr[8] = 8 // bit depth
  ihdr[9] = 6 // color type: truecolour with alpha
  ihdr[10] = 0 // deflate
  ihdr[11] = 0 // adaptive filtering
  ihdr[12] = 0 // no interlace

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ])
}

/* ---------------------------------------------------------------- render ---- */

/** One log-row in the icon, as fractions of the canvas. */
const BARS = [
  { y: 0.335, w: 0.44, h: 0.052 },
  { y: 0.455, w: 0.34, h: 0.052 },
  { y: 0.575, w: 0.40, h: 0.052 },
  { y: 0.695, w: 0.25, h: 0.052 },
]
const BAR_X = 0.28

/**
 * Point-in-rounded-rectangle test, in canvas fraction space.
 * @returns {boolean}
 */
function inRoundedRect(px, py, x0, y0, x1, y1, r) {
  if (px < x0 || px > x1 || py < y0 || py > y1) return false
  const cx = Math.min(Math.max(px, x0 + r), x1 - r)
  const cy = Math.min(Math.max(py, y0 + r), y1 - r)
  const dx = px - cx
  const dy = py - cy
  return dx * dx + dy * dy <= r * r
}

/**
 * Renders the icon at a given edge length with 4x4 supersampled coverage,
 * so small sizes (16px, 32px) stay clean without any blur or glow.
 * @param {number} size
 * @returns {Uint8Array}
 */
function render(size) {
  const out = new Uint8Array(size * size * 4)
  const SS = 4
  const samples = SS * SS
  const radius = 0.22

  for (let y = 0; y < size; y += 1) {
    for (let x = 0; x < size; x += 1) {
      let r = 0
      let g = 0
      let b = 0
      let a = 0

      for (let sy = 0; sy < SS; sy += 1) {
        for (let sx = 0; sx < SS; sx += 1) {
          const px = (x + (sx + 0.5) / SS) / size
          const py = (y + (sy + 0.5) / SS) / size

          if (!inRoundedRect(px, py, 0, 0, 1, 1, radius)) continue

          let bar = null
          for (const candidate of BARS) {
            const top = candidate.y
            const bottom = candidate.y + candidate.h
            const right = BAR_X + candidate.w
            // Bars are trimmed inward so they never touch the plate edge.
            if (
              inRoundedRect(
                px,
                py,
                BAR_X,
                top,
                right,
                bottom,
                candidate.h / 2,
              )
            ) {
              bar = candidate
              break
            }
          }

          const c = bar === null ? PLATE : BAR
          r += c[0]
          g += c[1]
          b += c[2]
          a += 255
        }
      }

      const i = (y * size + x) * 4
      // Weight colour by the covered subsamples only (straight alpha).
      const covered = a / 255
      if (covered === 0) continue
      out[i] = Math.round(r / covered)
      out[i + 1] = Math.round(g / covered)
      out[i + 2] = Math.round(b / covered)
      out[i + 3] = Math.round(a / samples)
    }
  }
  return out
}

/* ----------------------------------------------------------------- ICO ----- */

/**
 * Builds a PNG-compressed ICO container (supported since Windows Vista, which
 * covers every Tauri v2 target).
 * @param {number[]} sizes
 * @returns {Buffer}
 */
function buildIco(sizes) {
  const images = sizes.map((size) => ({
    size,
    png: encodePng(render(size), size, size),
  }))

  const header = Buffer.alloc(6)
  header.writeUInt16LE(0, 0) // reserved
  header.writeUInt16LE(1, 2) // type: icon
  header.writeUInt16LE(images.length, 4)

  const dir = Buffer.alloc(16 * images.length)
  let offset = 6 + dir.length
  images.forEach((image, index) => {
    const at = index * 16
    dir[at] = image.size >= 256 ? 0 : image.size // 0 means 256
    dir[at + 1] = image.size >= 256 ? 0 : image.size
    dir[at + 2] = 0 // palette size
    dir[at + 3] = 0 // reserved
    dir.writeUInt16LE(1, at + 4) // colour planes
    dir.writeUInt16LE(32, at + 6) // bits per pixel
    dir.writeUInt32LE(image.png.length, at + 8)
    dir.writeUInt32LE(offset, at + 12)
    offset += image.png.length
  })

  return Buffer.concat([header, dir, ...images.map((i) => i.png)])
}

/* ----------------------------------------------------------------- main ---- */

mkdirSync(ICON_DIR, { recursive: true })

/** @type {Array<[string, Buffer]>} */
const outputs = [
  ['32x32.png', encodePng(render(32), 32, 32)],
  ['128x128.png', encodePng(render(128), 128, 128)],
  ['128x128@2x.png', encodePng(render(256), 256, 256)],
  ['icon.png', encodePng(render(512), 512, 512)],
  ['icon.ico', buildIco([16, 24, 32, 48, 64, 128, 256])],
]

for (const [name, data] of outputs) {
  const target = join(ICON_DIR, name)
  writeFileSync(target, data)
  console.log(`wrote ${name} (${data.length} bytes)`)
}

// Windows Store / macOS style extras are omitted on purpose: the bundle targets
// configured in tauri.conf.json (msi, nsis) only consume the files above.
console.log(`\nicon set written to ${ICON_DIR}`)
