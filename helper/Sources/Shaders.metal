#include <metal_stdlib>
using namespace metal;

// Uniforms shared with Swift (64 bytes). The effect is anchored to `rect`,
// the window the assistant is working in (view coordinates, points, y up).
struct U {
    float2 res;        // view size in points
    float2 rectOrigin; // window rect origin (bottom-left)
    float2 rectSize;   // window rect size
    float time;
    float level;       // speaking loudness 0..1 (0 when silent)
    float vis;         // presence 0..1 (fades out when there is no window)
    float glance;      // 1 right after the assistant looked at this window, decays
    float work;        // 0..1 while tools are running
    float listen;      // 0..1 while the user talks
    float scale;       // backing scale
    float hasTex;      // 1 when a live screen texture is bound
    float talkMode;    // unused (kept so the layout matches the Swift struct)
    float levelFast;   // loudness with a snappy envelope (syllables)
    float voicePhase;  // clock that runs faster the louder the voice is
    float speak;       // 0..1 envelope: is it speaking at all
    float beat;        // double-pulse envelope fired by real syllable onsets
    float pad2;
};
struct VOut { float4 pos [[position]]; };

vertex VOut vmain(uint vid [[vertex_id]]) {
    float2 v = float2(float((vid << 1) & 2), float(vid & 2)) * 2.0 - 1.0;
    VOut o; o.pos = float4(v, 0.0, 1.0); return o;
}

// ---------- helpers ----------
constant float PI2 = 6.28318530718;
constant float CORNER = 18.0; // approximate macOS window corner radius (points); only shapes the falloff
constant float GAIN = 0.80;   // overall strength: 0.8 = the "about 20% weaker" setting

float hash21(float2 p) { p = fract(p * float2(123.34, 456.21)); p += dot(p, p + 45.32); return fract(p.x * p.y); }
float hash11(float n) { return fract(sin(n * 12.9898) * 43758.5453); }
float2 hash22(float2 p) { float n = hash21(p); return float2(n, hash21(p + n + 1.7)); }
float vnoise(float2 p) {
    float2 i = floor(p), f = fract(p); f = f * f * (3.0 - 2.0 * f);
    float a = hash21(i), b = hash21(i + float2(1.0, 0.0)), c = hash21(i + float2(0.0, 1.0)), d = hash21(i + float2(1.0, 1.0));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}
float fbm(float2 p) { float v = 0.0, a = 0.5; for (int i = 0; i < 4; i++) { v += a * vnoise(p); p = p * 2.03 + 17.1; a *= 0.5; } return v; }
float3 hsv(float h, float s, float v) {
    float3 k = float3(1.0, 2.0 / 3.0, 1.0 / 3.0);
    float3 p = abs(fract(float3(h) + k) * 6.0 - 3.0);
    return v * mix(float3(1.0), clamp(p - 1.0, 0.0, 1.0), s);
}
float3 pal(float t) { return 0.5 + 0.5 * cos(PI2 * (t + float3(0.0, 0.33, 0.67))); }
// Apple-ish iridescent palette: blue → violet → magenta → orange
float3 iris(float k) {
    float3 c0 = float3(0.25, 0.62, 1.00), c1 = float3(0.62, 0.38, 1.00), c2 = float3(1.00, 0.32, 0.70), c3 = float3(1.00, 0.62, 0.28);
    k = fract(k) * 4.0; float f = smoothstep(0.0, 1.0, fract(k)); int i = int(floor(k));
    float3 a = (i == 0) ? c0 : (i == 1) ? c1 : (i == 2) ? c2 : c3;
    float3 b = (i == 0) ? c1 : (i == 1) ? c2 : (i == 2) ? c3 : c0;
    return mix(a, b, f);
}
float sdRoundBox(float2 p, float2 b, float r) {
    float2 q = abs(p) - b + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}
float gauss(float x, float s) { return exp(-(x * x) / (2.0 * s * s)); }

struct F {
    float2 p;      // pixel in points, y up
    float2 c;      // window centre
    float2 hb;     // window half size
    float d;       // signed distance to the window edge (negative inside)
    float a;       // angle around the centre, 0..1
    float t, L, vis, glance, work, listen, E;
};
F frame(VOut in, constant U& u) {
    F f;
    f.p = in.pos.xy / u.scale; f.p.y = u.res.y - f.p.y;
    f.hb = u.rectSize * 0.5; f.c = u.rectOrigin + f.hb;
    f.d = sdRoundBox(f.p - f.c, f.hb, CORNER);
    f.a = fract(atan2(f.p.y - f.c.y, f.p.x - f.c.x) / PI2);
    f.t = u.time; f.L = u.level; f.vis = u.vis; f.glance = u.glance; f.work = u.work; f.listen = u.listen;
    // Overall energy: quiet presence, more when listening or working, most when speaking.
    f.E = clamp(0.10 + 0.20 * u.listen + 0.26 * u.work + 0.75 * u.level, 0.0, 1.0);
    return f;
}
float4 outc(float3 rgb, float a, constant U& u) { a = clamp(a, 0.0, 1.0) * u.vis; return float4(rgb * a, a); }
// Quick white-ish flash of the rim when the assistant just looked at the window.
float4 withGlance(float4 col, F f, constant U& u) {
    float g = f.glance * gauss(f.d, 5.0 + 14.0 * f.glance) * 0.9;
    float3 gc = float3(0.95, 0.97, 1.0) * g;
    return float4(col.rgb + gc * u.vis, clamp(col.a + g * u.vis, 0.0, 1.0));
}
#define FX(n) fragment float4 fx##n(VOut in [[stage_in]], constant U& u [[buffer(0)]], texture2d<float> screenTex [[texture(0)]])

constexpr sampler smp(filter::linear, address::clamp_to_edge);

// ---------- edge-colour extrapolation ("Ambilight for the window") ----------
// The colours in the window's outer few points are sampled from a live,
// low-resolution capture of the screen and extended outward.
struct Edge {
    float2 e;        // nearest point on the window edge
    float2 nrm;      // outward normal
    float2 tan;      // tangent along the edge
    float outside;   // distance outside the window (0 inside)
    float s;         // coordinate along the edge, for streak/flow noise
    float P;         // perimeter length
    float corner;    // 1 at a corner, easing to 0 about 60 points along either edge
};
Edge edgeOf(F f) {
    Edge g;
    float2 mn = f.c - f.hb, mx = f.c + f.hb;
    float2 lo = f.p - mn, hi = mx - f.p;
    if (lo.x > 0.0 && lo.y > 0.0 && hi.x > 0.0 && hi.y > 0.0) {
        // Inside the window's bounds (this includes the slivers between the
        // square bounds and the rounded corners): belong to the nearest edge,
        // so corner pixels continue the rays of the edge next to them.
        float m = min(min(lo.x, hi.x), min(lo.y, hi.y));
        if (m == lo.y)      { g.e = float2(f.p.x, mn.y); g.nrm = float2(0.0, -1.0); }
        else if (m == hi.x) { g.e = float2(mx.x, f.p.y); g.nrm = float2(1.0, 0.0); }
        else if (m == hi.y) { g.e = float2(f.p.x, mx.y); g.nrm = float2(0.0, 1.0); }
        else                { g.e = float2(mn.x, f.p.y); g.nrm = float2(-1.0, 0.0); }
    } else {
        g.e = clamp(f.p, mn, mx);
        float2 dv = f.p - g.e;
        float sq = length(dv);
        g.nrm = sq > 1e-3 ? dv / sq : float2(0.0, 1.0);
    }
    g.tan = float2(-g.nrm.y, g.nrm.x);
    g.outside = max(f.d, 0.0);
    // Continuous coordinate along the perimeter: bottom → right → top → left,
    // with a quarter arc of virtual radius RHO at every corner so that rays
    // fan smoothly around it instead of sharing one value.
    float W = f.hb.x * 2.0, H = f.hb.y * 2.0; float eps = 0.01;
    const float RHO = 70.0; float A = 1.5707963 * RHO;
    g.P = 2.0 * (W + H) + 4.0 * A;
    bool onB = g.e.y <= mn.y + eps, onT = g.e.y >= mx.y - eps, onL = g.e.x <= mn.x + eps, onR = g.e.x >= mx.x - eps;
    // Distance from the edge point to the nearest corner, measured along the edge.
    float2 toCorner = min(g.e - mn, mx - g.e);
    float alongEdge = (onB || onT) ? toCorner.x : toCorner.y;
    g.corner = 1.0 - smoothstep(0.0, 60.0, alongEdge);
    float2 dvc = f.p - g.e;
    if (onB && onR)      g.s = W + A * clamp(atan2(dvc.x, -dvc.y) / 1.5707963, 0.0, 1.0);                       // bottom-right arc
    else if (onR && onT) g.s = W + A + H + A * clamp(atan2(dvc.y, dvc.x) / 1.5707963, 0.0, 1.0);                // top-right arc
    else if (onT && onL) g.s = 2.0 * W + 2.0 * A + H + A * clamp(atan2(-dvc.x, dvc.y) / 1.5707963, 0.0, 1.0);   // top-left arc
    else if (onL && onB) g.s = 2.0 * W + 3.0 * A + 2.0 * H + A * clamp(atan2(-dvc.y, -dvc.x) / 1.5707963, 0.0, 1.0); // bottom-left arc
    else if (onB)        g.s = g.e.x - mn.x;
    else if (onR)        g.s = W + A + (g.e.y - mn.y);
    else if (onT)        g.s = W + 2.0 * A + H + (mx.x - g.e.x);
    else                 g.s = 2.0 * W + 3.0 * A + H + (mx.y - g.e.y);
    return g;
}

// ---------- Farbenlehre: a luminous colour from window edge + surroundings ----------
float3 rgb2hsv(float3 c) {
    float4 K = float4(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
    float4 p = mix(float4(c.bg, K.wz), float4(c.gb, K.xy), step(c.b, c.g));
    float4 q = mix(float4(p.xyw, c.r), float4(c.r, p.yzx), step(p.x, c.r));
    float d = q.x - min(q.w, q.y);
    return float3(abs(q.z + (q.w - q.y) / (6.0 * d + 1e-10)), d / (q.x + 1e-10), q.x);
}
float3 tapScreen(float2 q, constant U& u, texture2d<float> tex) {
    q = clamp(q, float2(1.0), u.res - 1.0);
    return tex.sample(smp, float2(q.x / u.res.x, 1.0 - q.y / u.res.y)).rgb;
}
// Colour of the window's outer few points, blurred along the edge. Many
// closely spaced taps plus a per-pixel jitter: with few wide taps, a bright
// window next to a dark one leaves one visible step per tap (banding).
float3 insideColor(F f, Edge g, constant U& u, texture2d<float> tex, float spread) {
    float3 acc = float3(0.0); float wsum = 0.0;
    float step_ = (5.0 + g.outside * 0.45) * spread * 0.5;
    float jitter = (hash21(f.p * 0.73) - 0.5) * step_;
    for (int k = -6; k <= 6; k++) {
        float fk = float(k); float w = exp(-fk * fk / 18.0);
        float2 q = clamp(g.e - g.nrm * (3.0 + abs(fk) * 1.3) + g.tan * (fk * step_ + jitter), f.c - f.hb + 1.5, f.c + f.hb - 1.5);
        acc += tapScreen(q, u, tex) * w; wsum += w;
    }
    return acc / wsum;
}
// Colour of what surrounds the window (wallpaper, neighbouring windows), just beyond the border.
float3 outsideColor(F f, Edge g, constant U& u, texture2d<float> tex, float spread) {
    float3 acc = float3(0.0); float wsum = 0.0;
    float step_ = (14.0 + g.outside * 0.3) * spread * 0.5;
    float jitter = (hash21(f.p * 0.91 + 3.7) - 0.5) * step_;
    for (int k = -4; k <= 4; k++) {
        float fk = float(k); float w = exp(-fk * fk / 10.0);
        float2 q = g.e + g.nrm * (22.0 + abs(fk) * 8.0) + g.tan * (fk * step_ + jitter);
        acc += tapScreen(q, u, tex) * w; wsum += w;
    }
    return acc / wsum;
}
struct Tone { float h; float s; float chroma; };
// Base tone from the window's border and what surrounds it, mixed as light
// (in RGB, widely blurred) so neighbouring colours blend instead of flipping.
// Surroundings count a little more. Where the scene is grey the light simply
// turns pale, keeping a slow drifting tint, rather than jumping to another hue.
Tone baseTone(F f, Edge g, constant U& u, texture2d<float> tex, float spread) {
    Tone t;
    float drift = fract(0.58 + 0.10 * sin(g.s * 0.0015 + u.voicePhase * 0.10));
    if (u.hasTex < 0.5) { t.h = drift; t.s = 0.55; t.chroma = 0.0; return t; }
    float3 ci = insideColor(f, g, u, tex, spread * 2.6);
    float3 co = outsideColor(f, g, u, tex, spread * 2.2);
    float3 hi = rgb2hsv(ci), ho = rgb2hsv(co);
    float wi = 0.15 + hi.y * hi.z, wo = (0.15 + ho.y * ho.z) * 1.35;
    float3 mixc = (ci * wi + co * wo) / (wi + wo);
    float3 hm = rgb2hsv(mixc);
    t.chroma = hm.y * max(hm.z, 0.35);
    float k = smoothstep(0.02, 0.10, t.chroma);
    float2 hv = mix(float2(cos(drift * PI2), sin(drift * PI2)), float2(cos(hm.x * PI2), sin(hm.x * PI2)), k);
    t.h = fract(atan2(hv.y, hv.x) / PI2);
    t.s = clamp(0.28 + t.chroma * 1.5, 0.28, 0.85);
    return t;
}
// Always luminous: full value, controlled saturation.
float3 lightOf(float h, float s) { return hsv(fract(h), s, 1.0); }

// Emit light: colour carries more than alpha so it adds light on dark
// surroundings and tints bright ones, never darkening anything.
float4 emit(F f, float3 rgbLight, float intensity, float3 rimCol, constant U& u) {
    // Drawn behind the window: light continues under its edge so the real
    // rounded corners reveal it seamlessly; the hidden interior is skipped.
    float on = step(-40.0, f.d);
    intensity = clamp(intensity, 0.0, 1.0) * on;
    float rim = 0.0; (void)rimCol; // the window in front is the edge; a drawn rim would never match its real corner radius
    float flash = f.glance * gauss(f.d, 5.0 + 16.0 * f.glance) * 0.9;
    float3 rgb = rgbLight * on + rimCol * rim + float3(0.97, 0.98, 1.0) * flash;
    float a = clamp(intensity * 0.62 + rim * 0.8 + flash, 0.0, 1.0);
    return float4(min(rgb, float3(1.0)) * u.vis, a * u.vis);
}
// ---------- talk animation: how the light reacts while the voice speaks ----------
struct Talk {
    float len;     // ray / bloom length multiplier
    float inten;   // brightness multiplier
    float hue;     // hue offset
    float freq;    // ray fineness multiplier (lower = thicker rays)
    float clock;   // time used for the ray noise
    float spread;  // hue dispersion along a ray
    float white;   // white-hot mix
    float bloom;   // extra soft bloom
};
Talk talkOf(F f, Edge g, constant U& u) {
    // Heartbeat: a lub-dub pulse fired by each real syllable of the voice; still when silent.
    Talk k; float L = f.L, b = u.beat;
    k.len = 1.0 + 1.6 * b + 0.25 * L; k.inten = 1.0 + 0.85 * b + 0.15 * L;
    k.hue = 0.0; k.freq = 1.0; k.clock = u.voicePhase * 0.9; k.spread = 0.28; k.white = 0.0; k.bloom = 0.0;
    return k;
}
// Lengths and strengths: quiet at rest, alive while speaking.
float rayLen(F f, Talk k, constant U& u)  { return (20.0 + 30.0 * (f.E - 0.75 * f.L) + 90.0 * u.speak) * k.len * (0.5 + 0.5 * GAIN); }
float reachOf(F f, Talk k, constant U& u) { return (14.0 + 26.0 * (f.E - 0.75 * f.L) + 54.0 * u.speak) * mix(1.0, k.len, 0.6) * (0.5 + 0.5 * GAIN); }
float strength(F f, Talk k, constant U& u) { return clamp((0.26 + 0.30 * (f.E - 0.75 * f.L) + 0.34 * u.speak) * k.inten * GAIN, 0.0, 1.0); }
float3 disperse(float o, float r) { return float3(exp(-o / (r * 0.8)), exp(-o / (r * 1.1)), exp(-o / (r * 1.5))); }
float rayNoise(Edge g, Talk k, float freq) {
    float fq = freq * k.freq;
    float fine = 0.25 * (1.0 - g.corner);
    float n = vnoise(float2(g.s * fq, k.clock)) * (1.0 - fine) + vnoise(float2(g.s * fq * 3.1, -k.clock * 1.3)) * fine;
    // Near a corner the rays would converge into spokes; ease them into an even glow there.
    return mix(n * n, 0.30, 0.75 * g.corner);
}
float3 tone3(Tone t, Talk k, float hueAdd, float sat) { return mix(lightOf(t.h + k.hue + hueAdd, sat), float3(1.0), k.white); }
float4 finishLight(F f, Edge g, Tone t, Talk k, float3 rgb, float i, constant U& u) {
    // optional soft bloom from the talk animation
    float b = k.bloom * exp(-g.outside / 46.0) * 0.5;
    rgb += tone3(t, k, 0.0, t.s * 0.6) * b; i += b;
    return emit(f, rgb, i, lightOf(t.h + k.hue, t.s * 0.7), u);
}

// Prism Rays: fine rays leave the window perpendicular to its edge, each one
// dispersing along its length from the scene's hue at the root to a shifted hue at the tip.
FX(0) { F f = frame(in, u); Edge g = edgeOf(f); if (f.d < -40.0) return float4(0.0); Talk k = talkOf(f, g, u); float maxLen = rayLen(f, k, u) * 1.1;
    if (g.outside > maxLen * 1.2 + 60.0) return float4(0.0);
    Tone t = baseTone(f, g, u, screenTex, 0.5);
    float n = rayNoise(g, k, 0.045); float len = maxLen * (0.25 + 0.75 * n);
    float along = clamp(g.outside / len, 0.0, 1.0);
    float i = pow(1.0 - along, 1.7) * strength(f, k, u);
    return finishLight(f, g, t, k, tone3(t, k, k.spread * along, mix(t.s, 0.55, along)) * i * 1.35, i, u); }
