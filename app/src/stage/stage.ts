/* =====================================================================
   Stage: the world behind the interface.
   Pass 1 renders the SSPI PS4 backdrop pattern (BackdropPattern.cs maths,
   animated). Pass 2 renders 3D game cases and download-card surfaces that
   track DOM anchors, so the DOM stays the single source of layout truth.
   A second transparent renderer above the DOM carries install flights.
   ===================================================================== */
import * as THREE from "three"
import { RoomEnvironment } from "three/examples/jsm/environments/RoomEnvironment.js"
import { PATTERN_STRENGTH, patternIndex } from "@/lib/appearance"
import { hexToRgb } from "@/lib/format"
import { E, Spring, clamp, damp, lerp, motionOK, onFrame } from "@/lib/motion"
import { CASE_ASPECT, blurredArt, caseFront, flowerMask, makeCanvas, placeholderFront, type CoverSpec } from "./art"

const BG_VERT = /* glsl */`
varying vec2 vUv;
void main() { vUv = uv; gl_Position = vec4(position.xy, 0.0, 1.0); }`

const BG_FRAG = /* glsl */`
precision highp float;
varying vec2 vUv;
uniform vec2 uRes;
uniform float uTime, uMotion, uMix, uDim, uTintAmt, uPaintR, uPaintOn, uStrength;
uniform int uPattern, uPrevPattern;
uniform vec3 uAccent, uPrevAccent, uTint;
uniform vec2 uMouse, uPaint;
uniform sampler2D uFlowers;

float hash12(vec2 p) { vec3 p3 = fract(vec3(p.xyx) * .1031); p3 += dot(p3, p3.yzx + 33.33); return fract((p3.x + p3.y) * p3.z); }

vec3 ps4Pattern(int p, vec2 uv, vec3 col, float t) {
  if (p == 5) return vec3((8.0 + 17.0 * (1.0 - uv.x) * (1.0 - uv.y)) / 255.0);
  if (p == 6) return vec3((5.0 + 10.0 * exp(-(pow(uv.x - .75, 2.0) + pow(uv.y - .9, 2.0)) * 4.0)) / 255.0);
  if (p == 7) return vec3((9.0 + 18.0 * exp(-(pow(uv.x - .2, 2.0) + pow(uv.y - .15, 2.0)) * 3.0)) / 255.0);
  vec3 base = vec3(11.0, 11.0, 12.0) / 255.0;
  if (p == 0) return base;
  if (p == 8) {
    float fx = uv.x - .82, fy = uv.y - .78;
    float fglow = .075 * exp(-(fx * fx + fy * fy) * 5.0) * (.85 + .15 * sin(t * .5));
    float ink = texture2D(uFlowers, uv).r;
    return vec3(9.0, 9.0, 11.0) / 255.0 + col * (fglow + ink) * uStrength;
  }
  vec2 c = vec2(.82, .85) + (uMouse - .5) * vec2(.05, .04);
  float dx = uv.x - c.x, dy = (uv.y - c.y) * .7;
  float radius = sqrt(dx * dx + dy * dy);
  float glow = exp(-radius * radius * 5.5);
  float wave = .5 + .5 * cos(radius * 38.0 - t * .6);
  if (p == 2) wave = .5 + .5 * sin(uv.x * 42.0 + sin(uv.y * 5.0 + t * .12) * 3.0 - t * .4);
  if (p == 3) { vec2 g = uv * vec2(640.0, 360.0) / 7.0; vec2 cell = floor(g) + smoothstep(.3, .7, fract(g)); wave = .25 + .75 * (.5 + .5 * sin(cell.x * .61 + cell.y * .32 - t * .5)); }
  if (p == 4) wave = exp(-pow((radius - .26 - .012 * sin(t * .45)) * 9.0, 2.0));
  vec2 m = fract(uv * vec2(640.0, 360.0) / 7.0);
  float line = 1.0 - smoothstep(0.0, .2, min(min(m.x, 1.0 - m.x), min(m.y, 1.0 - m.y)));
  float mesh = 1.0 - .08 * line;
  float intensity = (.025 + .22 * glow) * (.30 + .70 * wave) * mesh;
  return base + col * intensity * uStrength;
}

void main() {
  vec2 uv = vec2(gl_FragCoord.x / uRes.x, 1.0 - gl_FragCoord.y / uRes.y);
  float t = uTime * uMotion;
  vec3 accent = uAccent;
  float d = distance(gl_FragCoord.xy, uPaint);
  if (uPaintOn > .5) accent = mix(uPrevAccent, uAccent, smoothstep(uPaintR, uPaintR - 220.0, d));
  vec3 col = mix(accent, uTint, uTintAmt);
  vec3 c = ps4Pattern(uPattern, uv, col, t);
  if (uMix < .999) c = mix(ps4Pattern(uPrevPattern, uv, col, t), c, uMix);
  if (uPaintOn > .5) c += uAccent * exp(-pow((d - uPaintR) / 70.0, 2.0)) * .03;
  c *= uDim;
  c += (hash12(gl_FragCoord.xy + fract(uTime * 7.0) * 91.0) - .5) / 255.0;
  gl_FragColor = vec4(c, 1.0);
}`

/* Download cards: an opaque surface drawn behind the DOM card, so the card's 3D case stays
   visible above it. With the Artwork card style the card head (uBannerH px tall) is filled with
   the game's blurred art, darkest where the text sits; the Plain style keeps the flat surface
   and gives the open card a faint, mostly desaturated wash. */
const SURFACE_FRAG = /* glsl */`
precision highp float;
varying vec2 vUv;
uniform sampler2D uMap;
uniform float uOpacity, uAspect, uRadius, uArt, uHover, uHasMap, uBanner, uBannerH;
uniform vec2 uSize;
uniform vec4 uClip;
uniform vec3 uBase;
float roundedBox(vec2 p, vec2 b, float r) { vec2 q = abs(p) - b + r; return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r; }
void main() {
  vec2 uv = vUv;
  float texAspect = 640.0 / 360.0;
  vec2 st = uv;
  if (uAspect > texAspect) { st.y = .5 + (uv.y - .5) * texAspect / uAspect; } else { st.x = .5 + (uv.x - .5) * uAspect / texAspect; }
  vec3 art = texture2D(uMap, st).rgb;
  float lum = dot(art, vec3(.299, .587, .114));
  art = mix(vec3(lum), art, .4) * .6;
  vec3 surface = uBase + uHover * vec3(.016, .016, .018);
  float fromTop = (1.0 - uv.y) * uSize.y;
  float wash = smoothstep(.4, 1.0, uv.x) * (1.0 - smoothstep(80.0, 300.0, fromTop)) * uArt * uHasMap * .26 * (1.0 - uBanner);
  vec3 col = mix(surface, max(surface, art), wash);
  if (uBanner > .001 && uHasMap > .5) {
    float bh = max(uBannerH, 1.0);
    vec2 bst = vec2(uv.x, 1.0 - clamp(fromTop / bh, 0.0, 1.0));
    float bAspect = uSize.x / bh;
    if (bAspect > texAspect) { bst.y = .5 + (bst.y - .5) * texAspect / bAspect; } else { bst.x = .5 + (bst.x - .5) * bAspect / texAspect; }
    vec3 raw = texture2D(uMap, bst).rgb;
    float l = dot(raw, vec3(.299, .587, .114));
    vec3 banner = mix(vec3(l), raw, .85) * mix(.26, .5, smoothstep(.08, .95, uv.x)) + uHover * .02;
    float inBanner = 1.0 - smoothstep(bh - 22.0, bh + 1.0, fromTop);
    col = mix(col, banner, uBanner * inBanner);
  }
  vec2 p = (uv - .5) * uSize;
  float edge = 1.0 - smoothstep(-1.0, 1.0, roundedBox(p, uSize * .5 - 1.0, uRadius));
  float clip = step(uClip.y, gl_FragCoord.y) * step(gl_FragCoord.y, uClip.w) * step(uClip.x, gl_FragCoord.x) * step(gl_FragCoord.x, uClip.z);
  gl_FragColor = vec4(col, .965 * edge * uOpacity * clip);
}`

/* Retail case plastic, kept deep so the case edges read as blue without glowing. */
const CASE_BLUE = { side: 0x223f86, back: 0x1b336b }

export type CaseKind = "library" | "details" | "download"
export type CaseSpec = CoverSpec & { key: string; kind: CaseKind }

type ClipUniform = { value: THREE.Vector4 }
const OPEN_CLIP = () => new THREE.Vector4(-1e6, -1e6, 1e6, 1e6)

function buildCaseGeometry() {
  const w = 1, h = CASE_ASPECT, radius = 0.028, bevelSize = 0.007, bevelThickness = 0.011, depth = 0.082
  const hw = w / 2 - bevelSize, hh = h / 2 - bevelSize, r = radius
  const s = new THREE.Shape()
  s.moveTo(-hw + r, -hh); s.lineTo(hw - r, -hh); s.quadraticCurveTo(hw, -hh, hw, -hh + r)
  s.lineTo(hw, hh - r); s.quadraticCurveTo(hw, hh, hw - r, hh)
  s.lineTo(-hw + r, hh); s.quadraticCurveTo(-hw, hh, -hw, hh - r)
  s.lineTo(-hw, -hh + r); s.quadraticCurveTo(-hw, -hh, -hw + r, -hh)
  const g = new THREE.ExtrudeGeometry(s, { depth, bevelEnabled: true, bevelThickness, bevelSize, bevelSegments: 3, curveSegments: 5 })
  g.translate(0, 0, -depth / 2)
  const lid = g.groups[0], side = g.groups[1], half = lid.count / 2
  g.clearGroups()
  g.addGroup(lid.start, half, 2)
  g.addGroup(lid.start + half, half, 0)
  g.addGroup(side.start, side.count, 1)
  const pos = g.attributes.position, uv = g.attributes.uv
  for (let i = lid.start; i < lid.start + lid.count; i++) {
    let u = (pos.getX(i) + hw) / (2 * hw)
    const v = (pos.getY(i) + hh) / (2 * hh)
    if (i < lid.start + half) u = 1 - u
    uv.setXY(i, u, v)
  }
  uv.needsUpdate = true
  g.computeBoundingSphere()
  return { geometry: g, frontZ: depth / 2 + bevelThickness, thickness: depth + 2 * bevelThickness }
}

/** Vertical fade at a scroll container's edges, per material. */
function patchFade<M extends THREE.Material>(material: M, clip: ClipUniform, fade: { value: number }): M {
  material.onBeforeCompile = shader => {
    shader.uniforms.uClip = clip
    shader.uniforms.uFadePx = fade
    shader.fragmentShader = shader.fragmentShader
      .replace("#include <common>", "#include <common>\nuniform vec4 uClip;\nuniform float uFadePx;")
      .replace("#include <dithering_fragment>", "#include <dithering_fragment>\n gl_FragColor.a *= clamp(min(gl_FragCoord.y - uClip.y, uClip.w - gl_FragCoord.y) / uFadePx, 0.0, 1.0);")
  }
  material.customProgramCacheKey = () => "sspi-clipfade"
  return material
}

type Snapshot = { x: number; y: number; w: number }

function createStage() {
  const canvas = document.getElementById("stage") as HTMLCanvasElement | null
  const fxCanvas = document.getElementById("fx") as HTMLCanvasElement | null
  if (!canvas || !fxCanvas) return null
  let renderer: THREE.WebGLRenderer
  try {
    renderer = new THREE.WebGLRenderer({ canvas, antialias: true, powerPreference: "high-performance" })
  } catch (error) {
    console.warn("WebGL is unavailable; the interface uses flat cases.", error)
    return null
  }
  let W = window.innerWidth, H = window.innerHeight, DPR = Math.min(window.devicePixelRatio || 1, 2)
  renderer.setPixelRatio(DPR)
  renderer.autoClear = false
  renderer.outputColorSpace = THREE.SRGBColorSpace
  renderer.toneMapping = THREE.NeutralToneMapping
  renderer.setClearColor(0x0b0b0c, 1)
  const maxAniso = Math.min(8, renderer.capabilities.getMaxAnisotropy())

  /* ---------- pass 1: backdrop */
  const flowerData = new Uint8Array(640 * 360)
  const mask = flowerMask()
  for (let i = 0; i < mask.length; i++) flowerData[i] = Math.round(mask[i] * 255)
  const flowerTex = new THREE.DataTexture(flowerData, 640, 360, THREE.RedFormat, THREE.UnsignedByteType)
  flowerTex.magFilter = THREE.NearestFilter
  flowerTex.minFilter = THREE.LinearFilter
  flowerTex.needsUpdate = true
  const vec = (hex: string) => new THREE.Vector3(...hexToRgb(hex).map(v => v / 255))
  const bgUniforms = {
    uRes: { value: new THREE.Vector2(W * DPR, H * DPR) },
    uTime: { value: 0 }, uMotion: { value: 1 }, uMix: { value: 1 }, uDim: { value: 1 }, uStrength: { value: PATTERN_STRENGTH },
    uPattern: { value: 1 }, uPrevPattern: { value: 0 },
    uAccent: { value: vec("#E4E4E1") }, uPrevAccent: { value: vec("#E4E4E1") },
    uTint: { value: vec("#E4E4E1") }, uTintAmt: { value: 0 },
    uMouse: { value: new THREE.Vector2(0.5, 0.5) },
    uPaint: { value: new THREE.Vector2() }, uPaintR: { value: 0 }, uPaintOn: { value: 0 },
    uFlowers: { value: flowerTex },
  }
  const bgScene = new THREE.Scene()
  const bgCam = new THREE.OrthographicCamera(-1, 1, 1, -1, 0, 1)
  bgScene.add(new THREE.Mesh(new THREE.PlaneGeometry(2, 2), new THREE.ShaderMaterial({ uniforms: bgUniforms, vertexShader: BG_VERT, fragmentShader: BG_FRAG, depthTest: false, depthWrite: false })))

  /* ---------- pass 2: world in CSS pixels */
  const scene = new THREE.Scene()
  const camera = new THREE.PerspectiveCamera(28, W / H, 10, 20000)
  let camDist = 1
  const pmrem = new THREE.PMREMGenerator(renderer)
  scene.environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture
  const key = new THREE.DirectionalLight(0xffffff, 1.1)
  key.position.set(-0.35, 0.55, 1)
  scene.add(key)
  scene.add(new THREE.HemisphereLight(0xe8eeff, 0x141418, 0.45))

  const CASE = buildCaseGeometry()
  const planeGeo = new THREE.PlaneGeometry(1, 1)
  const shadowTex = (() => {
    const c = makeCanvas(128, 128), ctx = c.getContext("2d")!
    const g = ctx.createRadialGradient(64, 64, 0, 64, 64, 64)
    g.addColorStop(0, "rgba(0,0,0,.95)"); g.addColorStop(0.45, "rgba(0,0,0,.55)"); g.addColorStop(1, "rgba(0,0,0,0)")
    ctx.fillStyle = g
    ctx.fillRect(0, 0, 128, 128)
    return new THREE.CanvasTexture(c)
  })()

  const textures = new WeakMap<HTMLCanvasElement, THREE.CanvasTexture>()
  function tex(source: HTMLCanvasElement, colorSpace: THREE.ColorSpace = THREE.SRGBColorSpace) {
    let t = textures.get(source)
    if (!t) {
      t = new THREE.CanvasTexture(source)
      t.colorSpace = colorSpace
      t.anisotropy = maxAniso
      t.generateMipmaps = true
      t.minFilter = THREE.LinearMipmapLinearFilter
      textures.set(source, t)
    }
    return t
  }

  const viewportEl = () => document.getElementById("viewport")
  let viewRect = new DOMRect(0, 0, W, H)
  const rectToWorld = (rect: DOMRect) => ({ x: rect.left + rect.width / 2 - W / 2, y: H / 2 - (rect.top + rect.height / 2), w: rect.width })

  /** Device-pixel clip rect (GL coordinates) for the nearest scrolling list, or the whole view. */
  function clipFor(el: HTMLElement, out: THREE.Vector4) {
    const scroller = el.closest("[data-case-scroll]") as HTMLElement | null
    const r = scroller ? scroller.getBoundingClientRect() : viewRect
    out.set(r.left * DPR, (H - r.bottom) * DPR, r.right * DPR, (H - r.top) * DPR)
  }

  /* ---------- 3D game cases */
  const objects = new Set<CaseObj>()
  const byAnchor = new Map<HTMLElement, CaseObj>()
  const pendingFrom = new Map<string, { rect: DOMRect; at: number }>()

  class CaseObj {
    spec: CaseSpec
    anchor: HTMLElement | null = null
    clipEl: HTMLElement | null = null
    shared = false
    sharedAt = 0
    dead = false
    visible = false
    x = 0; y = 0; w = 0
    flightZ = 0
    clipVis = 1
    flight: { t: number; dur: number; from: Snapshot; arc: number } | null = null
    enterDelay = 0
    hovering = false
    focused = false
    textureWidth = 0
    readonly clip: ClipUniform = { value: OPEN_CLIP() }
    readonly fade = { value: 26 }
    readonly front: THREE.MeshPhysicalMaterial
    readonly side: THREE.MeshPhysicalMaterial
    readonly back: THREE.MeshPhysicalMaterial
    readonly shadowMat: THREE.MeshBasicMaterial
    readonly mesh: THREE.Mesh
    readonly shadow: THREE.Mesh
    readonly group = new THREE.Group()
    readonly tiltX = new Spring(0, 210, 20)
    readonly tiltY = new Spring(0, 210, 20)
    readonly lift = new Spring(0, 240, 22)
    readonly press = new Spring(0, 520, 30)
    readonly alpha = new Spring(0, 150, 24)

    constructor(spec: CaseSpec) {
      this.spec = spec
      const placeholder = placeholderFront(spec.titleId)
      const map = tex(placeholder.canvas)
      void placeholder.ready.then(() => { map.needsUpdate = true })
      this.front = patchFade(new THREE.MeshPhysicalMaterial({ map, emissive: 0xffffff, emissiveMap: map, emissiveIntensity: 0.36, roughness: 0.5, clearcoat: 0.9, clearcoatRoughness: 0.16, envMapIntensity: 0.32, transparent: true }), this.clip, this.fade)
      this.side = patchFade(new THREE.MeshPhysicalMaterial({ color: CASE_BLUE.side, roughness: 0.32, clearcoat: 0.8, clearcoatRoughness: 0.16, envMapIntensity: 0.55, transparent: true }), this.clip, this.fade)
      this.back = patchFade(new THREE.MeshPhysicalMaterial({ color: CASE_BLUE.back, roughness: 0.45, clearcoat: 0.8, clearcoatRoughness: 0.2, envMapIntensity: 0.4, transparent: true }), this.clip, this.fade)
      this.mesh = new THREE.Mesh(CASE.geometry, [this.front, this.side, this.back])
      this.mesh.renderOrder = 3
      this.shadowMat = patchFade(new THREE.MeshBasicMaterial({ map: shadowTex, transparent: true, depthWrite: false, opacity: 0, toneMapped: false }), this.clip, this.fade)
      this.shadow = new THREE.Mesh(planeGeo, this.shadowMat)
      this.shadow.renderOrder = 2
      this.group.add(this.shadow, this.mesh)
      scene.add(this.group)
    }
    /** Loads the cover texture the first time the case is on screen, sized for its role. */
    loadTexture() {
      const width = this.spec.kind === "details" ? 768 : 384
      if (width <= this.textureWidth) return
      this.textureWidth = width
      const spec = this.spec
      void caseFront(spec, width).then(canvas => {
        if (this.dead || spec !== this.spec) return
        const map = tex(canvas)
        this.front.map = map
        this.front.emissiveMap = map
        this.front.needsUpdate = true
      }).catch(() => undefined)
    }
    attach(anchor: HTMLElement, spec: CaseSpec) {
      if (this.anchor && byAnchor.get(this.anchor) === this) byAnchor.delete(this.anchor)
      this.anchor = anchor
      byAnchor.set(anchor, this)
      this.clipEl = anchor.closest("[data-case-clip]") as HTMLElement | null
      const coverChanged = spec.cover !== this.spec.cover || spec.title !== this.spec.title
      this.spec = spec
      if (coverChanged) this.textureWidth = 0
      if (this.visible) this.loadTexture()
    }
    kill() { this.dead = true; this.alpha.set(0) }
    dispose() {
      scene.remove(this.group)
      ;[this.front, this.side, this.back, this.shadowMat].forEach(m => m.dispose())
      objects.delete(this)
      if (this.anchor && byAnchor.get(this.anchor) === this) byAnchor.delete(this.anchor)
    }
    snapshot(): Snapshot { return { x: this.x, y: this.y, w: this.w } }
  }

  const cases = {
    register(anchor: HTMLElement, spec: CaseSpec, options: { delay?: number } = {}) {
      const existing = byAnchor.get(anchor)
      if (existing && !existing.dead) { existing.attach(anchor, spec); return }
      let obj: CaseObj | null = null
      for (const candidate of objects) {
        if (candidate.shared && !candidate.dead && candidate.spec.key === spec.key) { obj = candidate; break }
      }
      if (obj) {
        obj.shared = false
        const from = obj.snapshot()
        obj.attach(anchor, spec)
        obj.flight = motionOK() ? { t: 0, dur: 0.7, from, arc: 60 } : null
        obj.alpha.set(1)
        return
      }
      obj = new CaseObj(spec)
      obj.attach(anchor, spec)
      objects.add(obj)
      const from = pendingFrom.get(spec.key)
      if (from && performance.now() - from.at < 900 && motionOK()) {
        pendingFrom.delete(spec.key)
        const fr = rectToWorld(from.rect)
        obj.x = fr.x; obj.y = fr.y; obj.w = fr.w
        obj.alpha.snap(1)
        obj.flight = { t: 0, dur: 0.62, from: { x: fr.x, y: fr.y, w: fr.w }, arc: 40 }
      } else {
        obj.enterDelay = motionOK() ? options.delay || 0 : 0
        if (!motionOK()) obj.alpha.snap(1)
      }
    },
    unregister(anchor: HTMLElement) {
      const obj = byAnchor.get(anchor)
      if (!obj) return
      byAnchor.delete(anchor)
      obj.anchor = null
      if (obj.shared) { obj.sharedAt = performance.now(); return }
      obj.kill()
    },
    /** Keep the visible case for `key` alive so the next anchor with that key receives it (shared-element flight). */
    retain(key: string) {
      let best: CaseObj | null = null
      for (const obj of objects) {
        if (obj.dead || obj.spec.key !== key || !obj.visible) continue
        if (!best || obj.hovering || obj.w > best.w) best = obj
      }
      if (best) { best.shared = true; best.sharedAt = performance.now() }
      return !!best
    },
    /** The next case registered for `key` starts at `rect` (for example a result thumbnail). */
    expectFrom(key: string, rect: DOMRect) { pendingFrom.set(key, { rect, at: performance.now() }) },
    hover(anchor: HTMLElement, nx: number, ny: number) {
      const obj = byAnchor.get(anchor)
      if (!obj) return
      obj.hovering = true
      const strength = obj.spec.kind === "details" ? 0.6 : obj.spec.kind === "download" ? 0.7 : 1
      obj.tiltY.set(nx * 0.14 * strength)
      obj.tiltX.set(-ny * 0.1 * strength)
      obj.lift.set(1)
    },
    leave(anchor: HTMLElement) {
      const obj = byAnchor.get(anchor)
      if (!obj) return
      obj.hovering = false
      obj.tiltX.set(0); obj.tiltY.set(0); obj.lift.set(obj.focused ? 1 : 0); obj.press.set(0)
    },
    focus(anchor: HTMLElement, on: boolean) {
      const obj = byAnchor.get(anchor)
      if (!obj) return
      obj.focused = on
      obj.lift.set(on || obj.hovering ? 1 : 0)
    },
    press(anchor: HTMLElement, on: boolean) { byAnchor.get(anchor)?.press.set(on ? 1 : 0) },
    /** The case's current on-screen rectangle (it can lag the anchor during a flight). */
    worldRect(anchor: HTMLElement) {
      const obj = byAnchor.get(anchor)
      if (!obj || !obj.w) return anchor.getBoundingClientRect()
      const width = obj.w, height = obj.w * CASE_ASPECT
      return new DOMRect(obj.x + W / 2 - width / 2, H / 2 - obj.y - height / 2, width, height)
    },
  }

  /* Pointer delegation: any element with [data-hover-case] tilts the case inside it. */
  let hoverHost: Element | null = null
  const mouseTarget = new THREE.Vector2(0.5, 0.5)
  const anchorIn = (host: Element) => (host.querySelector("[data-case-anchor]") || host) as HTMLElement
  document.addEventListener("pointermove", event => {
    const host = (event.target as Element | null)?.closest?.("[data-hover-case]") || null
    if (hoverHost && hoverHost !== host) { cases.leave(anchorIn(hoverHost)); hoverHost = null }
    if (host) {
      const anchor = anchorIn(host)
      const r = anchor.getBoundingClientRect()
      if (r.width > 0) cases.hover(anchor, clamp(((event.clientX - r.left) / r.width - 0.5) * 2, -1.2, 1.2), clamp(((event.clientY - r.top) / r.height - 0.5) * 2, -1.2, 1.2))
      hoverHost = host
    }
    mouseTarget.set(event.clientX / W, event.clientY / H)
  }, { passive: true })
  document.addEventListener("pointerdown", event => {
    const host = (event.target as Element | null)?.closest?.("[data-hover-case]")
    if (host) cases.press(anchorIn(host), true)
  })
  document.addEventListener("pointerup", () => { for (const obj of objects) obj.press.set(0) })
  document.addEventListener("focusin", event => {
    const host = (event.target as Element | null)?.closest?.("[data-hover-case]")
    if (host) cases.focus(anchorIn(host), true)
  })
  document.addEventListener("focusout", event => {
    const host = (event.target as Element | null)?.closest?.("[data-hover-case]")
    if (host) cases.focus(anchorIn(host), false)
  })

  /* ---------- card surfaces */
  type Surface = { mesh: THREE.Mesh; material: THREE.ShaderMaterial; alpha: Spring; art: Spring; hover: Spring; banner: Spring; head: HTMLElement | null; radius: number; spec: CoverSpec }
  const surfaces = new Map<HTMLElement, Surface>()
  const blankTex = new THREE.DataTexture(new Uint8Array([17, 17, 19, 255]), 1, 1)
  blankTex.needsUpdate = true
  const surfacesApi = {
    register(el: HTMLElement, spec: CoverSpec) {
      const existing = surfaces.get(el)
      if (existing) { existing.alpha.set(1); return }
      const material = new THREE.ShaderMaterial({
        uniforms: {
          uMap: { value: blankTex }, uHasMap: { value: 0 }, uOpacity: { value: 0 }, uAspect: { value: 1 }, uRadius: { value: 12 },
          uSize: { value: new THREE.Vector2(100, 100) }, uClip: { value: OPEN_CLIP() }, uArt: { value: 0 }, uHover: { value: 0 },
          uBanner: { value: 0 }, uBannerH: { value: 100 },
          uBase: { value: new THREE.Vector3(17 / 255, 17 / 255, 19 / 255) },
        },
        vertexShader: BG_VERT.replace("gl_Position = vec4(position.xy, 0.0, 1.0);", "gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);"),
        fragmentShader: SURFACE_FRAG, transparent: true, depthWrite: false, depthTest: false,
      })
      const mesh = new THREE.Mesh(planeGeo, material)
      mesh.renderOrder = 1
      scene.add(mesh)
      const surface: Surface = {
        mesh, material, alpha: new Spring(0, 120, 22).set(1), art: new Spring(0, 90, 20), hover: new Spring(0, 260, 26),
        banner: new Spring(Number(el.dataset.banner ?? 0), 90, 20), head: el.querySelector<HTMLElement>(".dhead"),
        radius: parseFloat(getComputedStyle(el).borderTopLeftRadius) || 12, spec,
      }
      surfaces.set(el, surface)
      void blurredArt(spec).then(canvas => {
        if (!surfaces.has(el)) return
        material.uniforms.uMap.value = tex(canvas, THREE.NoColorSpace)
        material.uniforms.uHasMap.value = 1
      }).catch(() => undefined)
    },
    unregister(el: HTMLElement) {
      const surface = surfaces.get(el)
      if (surface) surface.alpha.set(0)
    },
  }

  /* ---------- global look: tint, accent paint, pattern crossfade */
  const mouse = new THREE.Vector2(0.5, 0.5)
  const tint = { current: new THREE.Vector3().copy(bgUniforms.uTint.value), target: new THREE.Vector3().copy(bgUniforms.uTint.value), amt: 0, amtTarget: 0 }
  let paint: { t: number; max: number } | null = null
  let patternFade: { t: number } | null = null
  let dimTarget = 1
  const look = {
    setTint(hex: string | null) {
      if (!hex) { tint.amtTarget = 0; return }
      const v = vec(hex), l = v.x * 0.299 + v.y * 0.587 + v.z * 0.114
      tint.target.copy(v).lerp(new THREE.Vector3(l, l, l), 0.45)
      tint.amtTarget = 0.45
    },
    setAccent(hex: string, origin?: { x: number; y: number }) {
      const next = vec(hex)
      if (!motionOK() || !origin) {
        bgUniforms.uAccent.value.copy(next); bgUniforms.uPrevAccent.value.copy(next); bgUniforms.uPaintOn.value = 0; paint = null
        return
      }
      bgUniforms.uPrevAccent.value.copy(bgUniforms.uAccent.value)
      bgUniforms.uAccent.value.copy(next)
      bgUniforms.uPaint.value.set(origin.x * DPR, (H - origin.y) * DPR)
      bgUniforms.uPaintOn.value = 1
      bgUniforms.uPaintR.value = 0
      paint = { t: 0, max: Math.hypot(W, H) * DPR * 1.1 }
    },
    setPattern(id: string, instant = false) {
      const next = patternIndex(id)
      if (next === bgUniforms.uPattern.value) return
      bgUniforms.uPrevPattern.value = bgUniforms.uPattern.value
      bgUniforms.uPattern.value = next
      const animate = motionOK() && !instant
      bgUniforms.uMix.value = animate ? 0 : 1
      patternFade = animate ? { t: 0 } : null
    },
    setDim(value: number, instant = false) { dimTarget = value; if (instant) bgUniforms.uDim.value = value },
  }

  /* ---------- FX overlay: install flights and particles above the DOM */
  const fxRenderer = new THREE.WebGLRenderer({ canvas: fxCanvas, antialias: true, alpha: true, premultipliedAlpha: true })
  fxRenderer.setClearColor(0x000000, 0)
  fxRenderer.outputColorSpace = THREE.SRGBColorSpace
  fxRenderer.toneMapping = THREE.NeutralToneMapping
  const fxScene = new THREE.Scene()
  const fxCam = new THREE.PerspectiveCamera(28, 1, 10, 20000)
  fxScene.environment = new THREE.PMREMGenerator(fxRenderer).fromScene(new RoomEnvironment(), 0.04).texture
  const fxKey = new THREE.DirectionalLight(0xffffff, 1.1)
  fxKey.position.set(-0.35, 0.55, 1)
  fxScene.add(fxKey)
  fxScene.add(new THREE.HemisphereLight(0xe8eeff, 0x141418, 0.45))
  let fxDirty = false
  const toWorld = (x: number, y: number) => new THREE.Vector3(x - W / 2, H / 2 - y, 0)

  const CAP = 1200
  const pos = new Float32Array(CAP * 3), col = new Float32Array(CAP * 3), size = new Float32Array(CAP), alpha = new Float32Array(CAP)
  const vel = new Float32Array(CAP * 3), life = new Float32Array(CAP), maxLife = new Float32Array(CAP), baseSize = new Float32Array(CAP)
  const drag = new Float32Array(CAP), grav = new Float32Array(CAP)
  const pGeo = new THREE.BufferGeometry()
  pGeo.setAttribute("position", new THREE.BufferAttribute(pos, 3))
  pGeo.setAttribute("color", new THREE.BufferAttribute(col, 3))
  pGeo.setAttribute("size", new THREE.BufferAttribute(size, 1))
  pGeo.setAttribute("alpha", new THREE.BufferAttribute(alpha, 1))
  const pMat = new THREE.ShaderMaterial({
    uniforms: { uScale: { value: 1 } },
    vertexShader: `uniform float uScale;
      attribute float size; attribute float alpha; attribute vec3 color;
      varying vec3 vColor; varying float vAlpha;
      void main() { vColor = color; vAlpha = alpha; gl_PointSize = size * uScale; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }`,
    fragmentShader: `varying vec3 vColor; varying float vAlpha;
      void main() { float d = length(gl_PointCoord - .5); float a = smoothstep(.5, .0, d); a *= a; gl_FragColor = vec4(vColor * a * vAlpha, a * vAlpha); }`,
    transparent: true, depthWrite: false, depthTest: false, blending: THREE.AdditiveBlending,
  })
  const points = new THREE.Points(pGeo, pMat)
  points.frustumCulled = false
  fxScene.add(points)
  let live = 0, cursor = 0
  function spawn(x: number, y: number, z: number, vx: number, vy: number, vz: number, rgb: number[], s: number, lifeSec: number, dragK = 1.6, gravity = 0) {
    const i = cursor
    cursor = (cursor + 1) % CAP
    if (life[i] <= 0) live++
    pos[i * 3] = x; pos[i * 3 + 1] = y; pos[i * 3 + 2] = z
    vel[i * 3] = vx; vel[i * 3 + 1] = vy; vel[i * 3 + 2] = vz
    col[i * 3] = rgb[0]; col[i * 3 + 1] = rgb[1]; col[i * 3 + 2] = rgb[2]
    baseSize[i] = s; life[i] = maxLife[i] = lifeSec; drag[i] = dragK; grav[i] = gravity
  }
  const rgbOf = (hex: string) => hexToRgb(hex).map(v => v / 255)
  const DUST = [0.34, 0.34, 0.36]
  const fx = {
    burst(x: number, y: number, hex = "#6f6f76", count = 10, spread = 0.35) {
      if (!motionOK()) return
      const w = toWorld(x, y), base = rgbOf(hex), white = [0.7, 0.7, 0.72]
      for (let i = 0; i < count; i++) {
        const a = Math.random() * Math.PI * 2, speed = (120 + Math.random() * 360) * spread
        spawn(w.x, w.y, 20, Math.cos(a) * speed, Math.sin(a) * speed + 60, (Math.random() - 0.5) * 200, Math.random() > 0.75 ? white : base, 3 + Math.random() * 6, 0.4 + Math.random() * 0.5, 3.2, 380)
      }
      fxDirty = true
    },
    shimmer(rect: DOMRect, hex = "#8a8a90") {
      if (!motionOK()) return
      const base = rgbOf(hex)
      for (let i = 0; i < 12; i++) {
        const w = toWorld(rect.left + Math.random() * rect.width, rect.top + rect.height * (0.3 + Math.random() * 0.7))
        spawn(w.x, w.y, 10, (Math.random() - 0.5) * 60, 40 + Math.random() * 120, 0, base, 2 + Math.random() * 4, 0.8 + Math.random() * 0.8, 1.2, -30)
      }
      fxDirty = true
    },
    flyCase(spec: CoverSpec, fromRect: DOMRect, toRect: DOMRect, options: { endWidth?: number; duration?: number } = {}) {
      return new Promise<void>(resolve => {
        if (!motionOK()) { resolve(); return }
        const placeholder = placeholderFront(spec.titleId)
        const map = tex(placeholder.canvas)
        void placeholder.ready.then(() => { map.needsUpdate = true })
        const front = new THREE.MeshPhysicalMaterial({ map, emissive: 0xffffff, emissiveMap: map, emissiveIntensity: 0.36, roughness: 0.5, clearcoat: 0.9, clearcoatRoughness: 0.16, envMapIntensity: 0.32, transparent: true })
        void caseFront(spec, 384).then(canvas => { const t = tex(canvas); front.map = t; front.emissiveMap = t; front.needsUpdate = true }).catch(() => undefined)
        const side = new THREE.MeshPhysicalMaterial({ color: CASE_BLUE.side, roughness: 0.32, clearcoat: 0.8, clearcoatRoughness: 0.16, envMapIntensity: 0.55, transparent: true })
        const back = new THREE.MeshPhysicalMaterial({ color: CASE_BLUE.back, roughness: 0.45, clearcoat: 0.8, transparent: true })
        const mesh = new THREE.Mesh(CASE.geometry, [front, side, back])
        fxScene.add(mesh)
        const from = toWorld(fromRect.left + fromRect.width / 2, fromRect.top + fromRect.height / 2)
        const to = toWorld(toRect.left + toRect.width / 2, toRect.top + toRect.height / 2)
        const c1 = new THREE.Vector3(from.x + (to.x - from.x) * 0.2, Math.max(from.y, to.y) + 90, 120)
        const c2 = new THREE.Vector3(to.x + (from.x - to.x) * 0.25, to.y + 60, 60)
        flights.push({ mesh, mats: [front, side, back], from, to, c1, c2, w0: fromRect.width, w1: options.endWidth || 24, t: 0, dur: options.duration || 0.95, resolve, trailAcc: 0, toRect })
        fxDirty = true
      })
    },
  }
  type Flight = { mesh: THREE.Mesh; mats: THREE.Material[]; from: THREE.Vector3; to: THREE.Vector3; c1: THREE.Vector3; c2: THREE.Vector3; w0: number; w1: number; t: number; dur: number; resolve: () => void; trailAcc: number; toRect: DOMRect }
  const flights: Flight[] = []
  const bez = (p0: THREE.Vector3, p1: THREE.Vector3, p2: THREE.Vector3, p3: THREE.Vector3, t: number, out: THREE.Vector3) => {
    const u = 1 - t
    return out.set(0, 0, 0).addScaledVector(p0, u * u * u).addScaledVector(p1, 3 * u * u * t).addScaledVector(p2, 3 * u * t * t).addScaledVector(p3, t * t * t)
  }
  const tmp = new THREE.Vector3(), tmp2 = new THREE.Vector3()
  function updateFx(dt: number) {
    if (!flights.length && live <= 0 && !fxDirty) return
    for (let i = flights.length - 1; i >= 0; i--) {
      const f = flights[i]
      f.t += dt / f.dur
      const p = Math.min(1, f.t), e = E.inOutCubic(p)
      bez(f.from, f.c1, f.c2, f.to, e, tmp)
      const w = lerp(f.w0, f.w1, E.inCubic(p) * 0.85 + p * 0.15)
      f.mesh.position.copy(tmp).setZ(tmp.z - CASE.frontZ * w)
      f.mesh.scale.setScalar(w)
      f.mesh.rotation.set(-0.12 * Math.sin(Math.PI * p), 0.1 * Math.sin(Math.PI * p), 0.08 * Math.sin(Math.PI * p))
      const fade = p > 0.88 ? 1 - (p - 0.88) / 0.12 : 1
      f.mats.forEach(m => { m.opacity = fade })
      f.trailAcc += dt
      while (f.trailAcc > 1 / 45) {
        f.trailAcc -= 1 / 45
        bez(f.from, f.c1, f.c2, f.to, Math.max(0, e - 0.015), tmp2)
        spawn(tmp2.x + (Math.random() - 0.5) * w * 0.25, tmp2.y + (Math.random() - 0.5) * w * 0.25, tmp2.z, (Math.random() - 0.5) * 30, (Math.random() - 0.5) * 30, 0, DUST, 3 + Math.random() * 4 * (1 - p * 0.6), 0.3 + Math.random() * 0.25, 2.5, 0)
      }
      if (f.t >= 1) {
        fxScene.remove(f.mesh)
        f.mats.forEach(m => m.dispose())
        flights.splice(i, 1)
        fx.burst(f.toRect.left + f.toRect.width / 2, f.toRect.top + f.toRect.height / 2)
        f.resolve()
      }
    }
    live = 0
    for (let i = 0; i < CAP; i++) {
      if (life[i] <= 0) { size[i] = 0; alpha[i] = 0; continue }
      life[i] -= dt
      if (life[i] <= 0) { size[i] = 0; alpha[i] = 0; continue }
      live++
      const k = Math.exp(-drag[i] * dt)
      vel[i * 3] *= k; vel[i * 3 + 1] = vel[i * 3 + 1] * k - grav[i] * dt; vel[i * 3 + 2] *= k
      pos[i * 3] += vel[i * 3] * dt; pos[i * 3 + 1] += vel[i * 3 + 1] * dt; pos[i * 3 + 2] += vel[i * 3 + 2] * dt
      const t = life[i] / maxLife[i]
      alpha[i] = Math.min(1, t * 1.6)
      size[i] = baseSize[i] * (0.4 + 0.6 * t)
    }
    ;(["position", "size", "alpha", "color"] as const).forEach(name => { pGeo.attributes[name].needsUpdate = true })
    pMat.uniforms.uScale.value = fxRenderer.getPixelRatio()
    fxDirty = flights.length > 0 || live > 0
    fxRenderer.render(fxScene, fxCam)
    if (!fxDirty) fxRenderer.clear()
  }

  /* ---------- boot: the eight shards of the Matrix mark assemble, catch a light sweep, then land in the header */
  const MATRIX_POLYS = [
    [[-26.2222, -27], [-3, -27], [-3, -10], [-6.1111, -6.8889]],
    [[-27, -26.2222], [-6.8889, -6.1111], [-10, -3], [-27, -3]],
    [[27, -26.2222], [27, -3], [10, -3], [6.8889, -6.1111]],
    [[26.2222, -27], [6.1111, -6.8889], [3, -10], [3, -27]],
    [[26.2222, 27], [3, 27], [3, 10], [6.1111, 6.8889]],
    [[27, 26.2222], [6.8889, 6.1111], [10, 3], [27, 3]],
    [[-27, 26.2222], [-27, 3], [-10, 3], [-6.8889, 6.1111]],
    [[-26.2222, 27], [-6.1111, 6.8889], [-3, 10], [-3, 27]],
  ]
  const MATRIX_TX = { a: 1.01, b: -0.105, c: 0.067, d: 1.01 }
  const bootScene = new THREE.Scene()
  bootScene.environment = scene.environment
  const bootLight = new THREE.DirectionalLight(0xffffff, 2.2)
  bootLight.position.set(-0.6, 0.8, 1)
  bootScene.add(bootLight)
  bootScene.add(new THREE.HemisphereLight(0xffffff, 0x222226, 0.6))
  const bootGroup = new THREE.Group()
  bootScene.add(bootGroup)
  const bootMaterial = new THREE.MeshPhysicalMaterial({ color: 0xd9d9dc, metalness: 0.92, roughness: 0.3, clearcoat: 0.6, clearcoatRoughness: 0.2, envMapIntensity: 1.35, transparent: true })
  const shards = MATRIX_POLYS.map(poly => {
    const pts = poly.map(([x, y]) => new THREE.Vector2(MATRIX_TX.a * x + MATRIX_TX.c * y, -(MATRIX_TX.b * x + MATRIX_TX.d * y)))
    const center = pts.reduce((c, p) => c.add(p), new THREE.Vector2()).divideScalar(pts.length)
    const geometry = new THREE.ExtrudeGeometry(new THREE.Shape(pts.map(p => p.clone().sub(center))), { depth: 5, bevelEnabled: true, bevelThickness: 0.9, bevelSize: 0.7, bevelSegments: 3 })
    geometry.translate(0, 0, -2.5)
    const mesh = new THREE.Mesh(geometry, bootMaterial)
    bootGroup.add(mesh)
    return { mesh, home: new THREE.Vector3(center.x, center.y, 0), from: new THREE.Vector3(), rot: new THREE.Euler() }
  })
  const boot = { t: -1, visible: false, assetsReady: false, landing: null as null | { t: number; to: { x: number; y: number }; toScale: number }, resolve: null as null | (() => void), brand: null as HTMLElement | null }
  const LOGO_PX = 200
  function updateBoot(dt: number) {
    if (boot.t < 0) return
    boot.t += dt
    const t = boot.t, scale = LOGO_PX / 54
    let gx = 0, gy = 0, gs = scale
    shards.forEach((s, i) => {
      const local = clamp((t - 0.12 - i * 0.05) / 0.8, 0, 1), e = E.outBack(local)
      s.mesh.position.lerpVectors(s.from, s.home, e)
      const r = 1 - E.outCubic(local)
      s.mesh.rotation.set(s.rot.x * r, s.rot.y * r, s.rot.z * r)
    })
    bootMaterial.opacity = clamp((t - 0.08) / 0.35, 0, 1)
    const sweep = clamp((t - 0.95) / 0.6, 0, 1)
    bootLight.position.set(-1.4 + sweep * 2.8, 0.8 - sweep * 0.3, 1)
    bootLight.intensity = 2.2 + Math.sin(sweep * Math.PI) * 2.5
    const spin = Math.sin(clamp((t - 0.2) / 1.4, 0, 1) * Math.PI) * 0.35
    if (t > 1.45 && boot.assetsReady && !boot.landing && boot.brand) {
      const brand = boot.brand.getBoundingClientRect()
      const to = rectToWorld(brand)
      boot.landing = { t: 0, to, toScale: brand.width / 60 }
      boot.resolve?.()
    }
    if (boot.landing) {
      boot.landing.t += dt
      const p = clamp(boot.landing.t / 0.7, 0, 1), e = E.inOutCubic(p)
      gx = lerp(0, boot.landing.to.x, e); gy = lerp(0, boot.landing.to.y, e); gs = lerp(scale, boot.landing.toScale, e)
      bootMaterial.opacity = 1 - clamp((p - 0.82) / 0.18, 0, 1)
      if (p >= 1) {
        boot.t = -1; boot.visible = false; boot.landing = null
        if (boot.brand) boot.brand.style.opacity = "1"
      }
    }
    bootGroup.position.set(gx, gy, 0)
    bootGroup.scale.setScalar(gs)
    bootGroup.rotation.set(-spin * 0.3, spin, 0)
    const caption = document.getElementById("bootCaption")
    if (caption) caption.style.opacity = String(boot.landing ? 0 : clamp((t - 0.35) / 0.4, 0, 0.85))
  }
  const bootApi = {
    /** Plays the intro until `ready` settles, then lands the mark on `brand`. Resolves when the interface may appear. */
    run(ready: Promise<unknown>, brand: HTMLElement | null) {
      if (!motionOK()) return ready.then(() => undefined, () => undefined)
      let seed = Date.now() % 1e6
      const rand = () => { seed = (seed * 16807) % 2147483647; return seed / 2147483647 }
      shards.forEach((s, i) => {
        const a = (i / shards.length) * Math.PI * 2 + rand() * 0.6
        s.from.set(Math.cos(a) * (70 + rand() * 60), Math.sin(a) * (70 + rand() * 60), 60 + rand() * 90)
        s.rot.set((rand() - 0.5) * 4, (rand() - 0.5) * 4, (rand() - 0.5) * 3)
      })
      boot.t = 0; boot.visible = true; boot.assetsReady = false; boot.landing = null; boot.brand = brand
      if (brand) brand.style.opacity = "0"
      look.setDim(0.35, true)
      look.setDim(1)
      const reveal = new Promise<void>(resolve => { boot.resolve = resolve })
      const started = performance.now()
      const done = () => {
        boot.assetsReady = true
        // If frames stall (a minimised or hidden window), don't hold the interface back.
        window.setTimeout(() => {
          if (boot.landing || boot.t < 0) return
          boot.t = -1; boot.visible = false
          if (boot.brand) boot.brand.style.opacity = "1"
          const caption = document.getElementById("bootCaption")
          if (caption) caption.style.opacity = "0"
          look.setDim(1, true)
          boot.resolve?.()
        }, Math.max(600, 2600 - (performance.now() - started)))
      }
      ready.then(done, done)
      window.setTimeout(done, 8000)
      return reveal
    },
  }

  /* ---------- sizing and the frame loop */
  function resize() {
    W = window.innerWidth; H = window.innerHeight; DPR = Math.min(window.devicePixelRatio || 1, 2)
    renderer.setPixelRatio(DPR)
    renderer.setSize(W, H, false)
    camera.aspect = W / H
    camDist = (H / 2) / Math.tan(THREE.MathUtils.degToRad(camera.fov / 2))
    camera.position.set(0, 0, camDist); camera.near = camDist * 0.08; camera.far = camDist * 6
    camera.lookAt(0, 0, 0); camera.updateProjectionMatrix()
    bgUniforms.uRes.value.set(W * DPR, H * DPR)
    fxRenderer.setPixelRatio(DPR)
    fxRenderer.setSize(W, H, false)
    fxCam.aspect = W / H
    fxCam.position.set(0, 0, camDist); fxCam.near = camDist * 0.05; fxCam.far = camDist * 6
    fxCam.lookAt(0, 0, 0); fxCam.updateProjectionMatrix()
  }
  resize()
  window.addEventListener("resize", resize)

  function update(dt: number, time: number) {
    const vp = viewportEl()
    viewRect = vp ? vp.getBoundingClientRect() : new DOMRect(0, 0, W, H)
    const motion = motionOK()
    bgUniforms.uTime.value = time
    bgUniforms.uMotion.value = damp(bgUniforms.uMotion.value, motion ? 1 : 0, 3, dt)
    mouse.x = damp(mouse.x, mouseTarget.x, 2.2, dt); mouse.y = damp(mouse.y, mouseTarget.y, 2.2, dt)
    bgUniforms.uMouse.value.set(motion ? mouse.x : 0.5, motion ? mouse.y : 0.5)
    key.position.set(-0.35 + (mouse.x - 0.5) * 0.6, 0.55 - (mouse.y - 0.5) * 0.5, 1)
    tint.amt = damp(tint.amt, tint.amtTarget, 2.6, dt)
    tint.current.lerp(tint.target, 1 - Math.exp(-3.2 * dt))
    bgUniforms.uTint.value.copy(tint.current)
    bgUniforms.uTintAmt.value = tint.amt
    bgUniforms.uDim.value = damp(bgUniforms.uDim.value, dimTarget, 2.4, dt)
    if (paint) {
      paint.t += dt
      bgUniforms.uPaintR.value = E.outCubic(Math.min(1, paint.t / 1.25)) * paint.max
      if (paint.t > 1.3) { bgUniforms.uPaintOn.value = 0; bgUniforms.uPrevAccent.value.copy(bgUniforms.uAccent.value); paint = null }
    }
    if (patternFade) {
      patternFade.t += dt
      bgUniforms.uMix.value = E.inOutCubic(Math.min(1, patternFade.t / 1.1))
      if (patternFade.t >= 1.1) patternFade = null
    }

    const top = viewRect.top - 400, bottom = viewRect.bottom + 400
    const now = performance.now()
    for (const obj of [...objects]) {
      if (obj.shared && !obj.anchor && now - obj.sharedAt > 700) { obj.shared = false; obj.kill() }
      if (obj.enterDelay > 0) { obj.enterDelay -= dt; if (obj.enterDelay <= 0) obj.alpha.set(1) }
      else if (!obj.dead && obj.alpha.target === 0) obj.alpha.set(1)
      const connected = !!obj.anchor && obj.anchor.isConnected
      if (!connected && !obj.shared && !obj.dead) obj.kill()
      if (connected && !obj.dead && obj.anchor) {
        const r = obj.anchor.getBoundingClientRect()
        const t = rectToWorld(r)
        if (obj.flight) {
          const f = obj.flight
          f.t += dt / f.dur
          const p = Math.min(1, f.t), e = E.inOutCubic(p)
          obj.x = lerp(f.from.x, t.x, e); obj.y = lerp(f.from.y, t.y, e); obj.w = lerp(f.from.w, t.w, e)
          obj.flightZ = Math.sin(Math.PI * e) * f.arc
          if (p >= 1) { obj.flight = null; obj.flightZ = 0 }
        } else { obj.x = t.x; obj.y = t.y; obj.w = t.w; obj.flightZ = 0 }
        obj.clipVis = 1
        if (obj.clipEl) {
          const cr = obj.clipEl.getBoundingClientRect()
          obj.clipVis = clamp(Math.min(r.right - cr.left, cr.right - r.left) / (r.width * 0.55), 0, 1)
        }
        clipFor(obj.anchor, obj.clip.value)
        obj.visible = r.bottom > top && r.top < bottom && r.width > 2 && obj.clipVis > 0
        if (obj.visible) obj.loadTexture()
      }
      obj.tiltX.step(dt); obj.tiltY.step(dt); obj.lift.step(dt); obj.press.step(dt)
      const a = obj.alpha.step(dt)
      if (obj.dead && a < 0.01) { obj.dispose(); continue }
      obj.group.visible = obj.visible && a > 0.005
      if (!obj.group.visible) continue
      const s = obj.w, lift = obj.lift.value, press = obj.press.value
      const z = -CASE.frontZ * s + lift * s * 0.06 - press * s * 0.03 + obj.flightZ
      const liftY = lift * 3 - press
      const enter = obj.dead ? 1 : 0.96 + 0.04 * clamp(a, 0, 1)
      obj.mesh.scale.setScalar(s * enter)
      obj.group.position.set(obj.x, obj.y + liftY, z)
      obj.mesh.rotation.set(obj.tiltX.value, obj.tiltY.value, 0)
      obj.shadow.position.set(s * 0.02, -s * 0.05 - lift * s * 0.04, -CASE.thickness * s * 0.5 - 2)
      obj.shadow.scale.set(s * (1.28 + lift * 0.12), s * CASE_ASPECT * (1.18 + lift * 0.1), 1)
      const opacity = clamp(a, 0, 1) * obj.clipVis
      obj.front.opacity = obj.side.opacity = obj.back.opacity = opacity
      obj.shadowMat.opacity = opacity * (0.34 + lift * 0.22)
    }

    for (const [el, surface] of surfaces) {
      const u = surface.material.uniforms
      if (!el.isConnected || surface.alpha.target === 0) {
        surface.alpha.set(0)
        if (surface.alpha.value < 0.01) { scene.remove(surface.mesh); surface.material.dispose(); surfaces.delete(el); continue }
      } else {
        surface.art.set(Number(el.dataset.artLevel ?? 0))
        surface.banner.set(Number(el.dataset.banner ?? 0))
        surface.hover.set(el.matches(":hover:not(.is-open)") ? 1 : 0)
      }
      surface.alpha.step(dt); surface.art.step(dt); surface.hover.step(dt); surface.banner.step(dt)
      const r = el.getBoundingClientRect()
      if (el.isConnected && r.width > 0) {
        const z = -CASE.thickness * 300 - 20, k = (camDist - z) / camDist
        surface.mesh.position.set((r.left + r.width / 2 - W / 2) * k, (H / 2 - r.top - r.height / 2) * k, z)
        surface.mesh.scale.set(r.width * k, r.height * k, 1)
        u.uAspect.value = r.width / Math.max(1, r.height)
        u.uSize.value.set(r.width, r.height)
        u.uRadius.value = surface.radius
        clipFor(el, u.uClip.value)
      }
      u.uOpacity.value = clamp(surface.alpha.value, 0, 1)
      u.uArt.value = clamp(surface.art.value, 0, 1)
      u.uHover.value = clamp(surface.hover.value, 0, 1)
      u.uBanner.value = clamp(surface.banner.value, 0, 1)
      u.uBannerH.value = surface.head?.offsetHeight || u.uSize.value.y
    }
    updateBoot(dt)
    updateFx(dt)
  }

  function render() {
    renderer.setScissorTest(false)
    renderer.setViewport(0, 0, W, H)
    renderer.clear(true, true, false)
    renderer.render(bgScene, bgCam)
    const v = viewRect
    renderer.setScissorTest(true)
    renderer.setScissor(v.left, H - v.bottom, v.width, v.height)
    renderer.clearDepth()
    // The DOM is hidden during the logo intro, but this canvas lives behind it.
    // Keep its tracked artwork out of the splash while preserving the backdrop and boot mark.
    if (!document.body.classList.contains("booting") && !boot.visible) renderer.render(scene, camera)
    renderer.setScissorTest(false)
    if (boot.visible) { renderer.clearDepth(); renderer.render(bootScene, camera) }
  }

  onFrame((dt, now) => {
    update(dt, now / 1000)
    render()
  })

  return { cases, surfaces: surfacesApi, look, fx, boot: bootApi }
}

export type Stage = NonNullable<ReturnType<typeof createStage>>

let instance: Stage | null | undefined
/** The shared stage, created on first use; null when WebGL is unavailable. */
export function getStage(): Stage | null {
  if (instance === undefined) {
    try { instance = createStage() } catch (error) { console.error(error); instance = null }
    if (!instance) document.documentElement.classList.add("no-webgl")
  }
  return instance
}
