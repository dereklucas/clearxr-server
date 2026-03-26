#version 450

// ------------------------------------------------------------
//  Clear XR – Procedural Space Scene + Cyberpunk Logo
//
//  Designed to showcase foveated rendering quality:
//   • High-frequency star fields and fine detail at gaze center
//   • SDF-raymarched crystalline geometry with crisp specular highlights
//   • Animated nebula haze and floating octahedra
//   • Floating holographic ClearXR logo panel
// ------------------------------------------------------------

layout(location = 0) in vec2 frag_uv;

layout(push_constant) uniform PC {
    vec4 cam_pos;       // xyz = world position,  w = time (seconds)
    vec4 cam_right;     // xyz = right vector,     w = eye/view index
    vec4 cam_up;        // xyz = up vector,        w = unused
    vec4 cam_fwd;       // xyz = forward vector,   w = unused
    vec4 fov;           // x = tan(left), y = tan(right), z = tan(down), w = tan(up)
} pc;

layout(set = 0, binding = 0) uniform HandUBO {
    vec4 joints[52];        // [0..25] left hand, [26..51] right hand
    vec4 hand_active;       // x=left_hand, y=right_hand, z=left_ctrl, w=right_ctrl
    vec4 ctrl_grip[2];      // xyz=position, w=radius
    vec4 ctrl_aim_pos[2];   // xyz=position
    vec4 ctrl_aim_dir[2];   // xyz=direction
    vec4 ctrl_inputs[2];    // x=trigger, y=squeeze, z=stick_x, w=stick_y
    vec4 ctrl_buttons[2];   // x=btn1_touch, y=btn2_touch, z=stick_click, w=menu_click
    vec4 ctrl_clicks[2];    // x=btn1_click (A/X), y=btn2_click (B/Y), z=0, w=0
    vec4 ctrl_touches[2];   // x=trigger_touch, y=squeeze_touch, z=thumbstick_touch, w=0
    vec4 ctrl_grip_right[2];// grip pose right vector
    vec4 ctrl_grip_up[2];   // grip pose up vector
} hands;

layout(location = 0) out vec4 out_color;

// ============================================================
// Utility
// ============================================================

float hash11(float p) {
    p = fract(p * 0.1031);
    p *= p + 33.33;
    p *= p + p;
    return fract(p);
}

float hash12(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

vec3 hash33(vec3 p3) {
    p3 = fract(p3 * vec3(0.1031, 0.1030, 0.0973));
    p3 += dot(p3, p3.yxz + 33.33);
    return fract((p3.xxy + p3.yxx) * p3.zyx);
}

float valueNoise(vec3 p) {
    vec3 i = floor(p);
    vec3 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(mix(hash12(i.xy),             hash12(i.xy + vec2(1, 0)), f.x),
            mix(hash12(i.xy + vec2(0, 1)), hash12(i.xy + vec2(1, 1)), f.x), f.y),
        mix(mix(hash12(i.xy + i.z),             hash12(i.xy + vec2(1, 0) + i.z), f.x),
            mix(hash12(i.xy + vec2(0, 1) + i.z), hash12(i.xy + vec2(1, 1) + i.z), f.x), f.y),
        f.z);
}

float fbm(vec3 p) {
    float v = 0.0, amp = 0.5;
    for (int i = 0; i < 4; i++) {
        v += amp * valueNoise(p);
        p  *= 2.1;
        amp *= 0.5;
    }
    return v;
}

// ============================================================
// SDF primitives
// ============================================================

float sdOctahedron(vec3 p, float s) {
    p = abs(p);
    return (p.x + p.y + p.z - s) * 0.57735027;
}

float sdSphere(vec3 p, float r) { return length(p) - r; }

float sdBox(vec3 p, vec3 b) {
    vec3 q = abs(p) - b;
    return length(max(q, 0.0)) + min(max(q.x, max(q.y, q.z)), 0.0);
}

float smin(float a, float b, float k) {
    float h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}

// 2D line segment SDF (for logo text rendering)
float sdSeg2D(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a, ba = b - a;
    float h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

// ============================================================
// Analytic ray intersections for hand skeleton rendering
// ============================================================

// Ray-sphere intersection. Returns t (distance along ray) or -1.0 if no hit.
float raySphere(vec3 ro, vec3 rd, vec3 center, float radius) {
    vec3 oc = ro - center;
    float b = dot(oc, rd);
    float c = dot(oc, oc) - radius * radius;
    float disc = b * b - c;
    if (disc < 0.0) return -1.0;
    float t = -b - sqrt(disc);
    return t > 0.0 ? t : -1.0;
}

// Ray-capsule intersection (sphere-swept line segment).
// Returns nearest t > 0 or -1.0 if no hit.
float rayCapsule(vec3 ro, vec3 rd, vec3 a, vec3 b, float radius) {
    float tMin = -1.0;

    // Spheres at endpoints
    float t1 = raySphere(ro, rd, a, radius);
    float t2 = raySphere(ro, rd, b, radius);
    if (t1 > 0.0) tMin = t1;
    if (t2 > 0.0 && (tMin < 0.0 || t2 < tMin)) tMin = t2;

    // Cylinder between a and b
    vec3 ab = b - a;
    float abLen2 = dot(ab, ab);
    if (abLen2 > 0.0001) {
        vec3 ao = ro - a;
        vec3 abN = ab / abLen2;
        vec3 d = rd - ab * dot(rd, abN);
        vec3 o = ao - ab * dot(ao, abN);
        float A = dot(d, d);
        float B = 2.0 * dot(d, o);
        float C = dot(o, o) - radius * radius;
        float disc = B * B - 4.0 * A * C;
        if (disc >= 0.0) {
            float t = (-B - sqrt(disc)) / (2.0 * A);
            if (t > 0.0) {
                vec3 p = ro + rd * t;
                float proj = dot(p - a, ab) / abLen2;
                if (proj >= 0.0 && proj <= 1.0) {
                    if (tMin < 0.0 || t < tMin) tMin = t;
                }
            }
        }
    }

    return tMin;
}

// ============================================================
// Logo panel constants
// ============================================================
const vec3  PANEL_POS  = vec3(0.0, 1.6, -3.5);
const vec3  PANEL_HALF = vec3(1.4, 0.5, 0.003);

// ============================================================
// ClearXR Logo – Cyberpunk SDF text
//
// Characters defined as line segments in a [0,1] x [0,1.4] cell.
// Angular, blocky letterforms with sharp geometry.
// ============================================================

float charSDF(vec2 p, int ch) {
    float d = 1e9;

    if (ch == 0) { // C
        d = min(d, sdSeg2D(p, vec2(0.9, 1.4), vec2(0.15, 1.4)));
        d = min(d, sdSeg2D(p, vec2(0.15, 1.4), vec2(0.0, 1.2)));  // top-left bevel
        d = min(d, sdSeg2D(p, vec2(0.0, 1.2), vec2(0.0, 0.2)));
        d = min(d, sdSeg2D(p, vec2(0.0, 0.2), vec2(0.15, 0.0)));  // bottom-left bevel
        d = min(d, sdSeg2D(p, vec2(0.15, 0.0), vec2(0.9, 0.0)));
    }
    else if (ch == 1) { // L
        d = min(d, sdSeg2D(p, vec2(0.0, 1.4), vec2(0.0, 0.2)));
        d = min(d, sdSeg2D(p, vec2(0.0, 0.2), vec2(0.15, 0.0)));  // bevel
        d = min(d, sdSeg2D(p, vec2(0.15, 0.0), vec2(0.9, 0.0)));
    }
    else if (ch == 2) { // E
        d = min(d, sdSeg2D(p, vec2(0.15, 1.4), vec2(0.9, 1.4)));
        d = min(d, sdSeg2D(p, vec2(0.0, 1.2), vec2(0.15, 1.4)));  // bevel
        d = min(d, sdSeg2D(p, vec2(0.0, 1.2), vec2(0.0, 0.2)));
        d = min(d, sdSeg2D(p, vec2(0.0, 0.2), vec2(0.15, 0.0)));  // bevel
        d = min(d, sdSeg2D(p, vec2(0.15, 0.0), vec2(0.9, 0.0)));
        d = min(d, sdSeg2D(p, vec2(0.1, 0.7), vec2(0.75, 0.7)));  // middle bar
    }
    else if (ch == 3) { // A
        d = min(d, sdSeg2D(p, vec2(0.15, 1.4), vec2(0.85, 1.4)));
        d = min(d, sdSeg2D(p, vec2(0.0, 1.2), vec2(0.15, 1.4)));  // top-left bevel
        d = min(d, sdSeg2D(p, vec2(0.85, 1.4), vec2(1.0, 1.2)));  // top-right bevel
        d = min(d, sdSeg2D(p, vec2(0.0, 1.2), vec2(0.0, 0.0)));
        d = min(d, sdSeg2D(p, vec2(1.0, 1.2), vec2(1.0, 0.0)));
        d = min(d, sdSeg2D(p, vec2(0.1, 0.65), vec2(0.9, 0.65))); // crossbar
    }
    else if (ch == 4) { // R
        d = min(d, sdSeg2D(p, vec2(0.15, 1.4), vec2(0.85, 1.4)));
        d = min(d, sdSeg2D(p, vec2(0.0, 1.2), vec2(0.15, 1.4)));  // top-left bevel
        d = min(d, sdSeg2D(p, vec2(0.85, 1.4), vec2(1.0, 1.2)));  // top-right bevel
        d = min(d, sdSeg2D(p, vec2(0.0, 1.2), vec2(0.0, 0.0)));
        d = min(d, sdSeg2D(p, vec2(1.0, 1.2), vec2(1.0, 0.7)));
        d = min(d, sdSeg2D(p, vec2(0.1, 0.7), vec2(1.0, 0.7)));   // middle bar
        d = min(d, sdSeg2D(p, vec2(0.55, 0.7), vec2(1.0, 0.0)));  // diagonal leg
    }
    else if (ch == 6) { // X
        d = min(d, sdSeg2D(p, vec2(0.0, 0.0), vec2(1.0, 1.4)));
        d = min(d, sdSeg2D(p, vec2(1.0, 0.0), vec2(0.0, 1.4)));
    }
    // ch == 5 → space → d stays at 1e9

    return d;
}

// Render the full ClearXR logo at a panel UV in [-1, 1].
vec3 renderLogo(vec2 puv, float time) {
    // ---- Colour palette ----
    vec3 cyan    = vec3(0.7, 0.92, 1.0);   // cool white-blue for CLEAR
    vec3 magenta = vec3(1.0, 0.0, 0.45);   // hot pink for XR
    vec3 white   = vec3(1.0);

    // ---- Map panel UV → text coordinate space ----
    // "CLEAR XR" = 8 chars.  Each cell is ~1.1 units wide, total ≈ 8.8.
    // Map UV.x [-1,1] so that text is centred horizontally.
    float charW  = 1.15;
    float totalW = 8.0 * charW;          // 9.2 text units
    float scaleX = totalW / 1.7;         // UV range used for text (±0.85)
    float scaleY = 1.4 / 0.45;           // maps ±0.225 in UV to 0..1.4

    vec2 tp = vec2(
        (puv.x + 0.85) * scaleX,
        (puv.y + 0.25) * scaleY
    );

    // ---- Evaluate each character SDF ----
    //  C=0  L=1  E=2  A=3  R=4  _=5  X=6  R=4
    int chars[8] = int[8](0, 1, 2, 3, 4, 5, 6, 4);

    float clearDist = 1e9;   // distance to "CLEAR"
    float xrDist    = 1e9;   // distance to "XR"

    for (int i = 0; i < 8; i++) {
        vec2 cp = tp - vec2(float(i) * charW + 0.08, 0.0);
        float d = charSDF(cp, chars[i]);
        if (i < 5)  clearDist = min(clearDist, d);
        else        xrDist    = min(xrDist, d);
    }

    // ---- Text rendering (no glow — sharp fills only) ----
    float thick = 0.055;
    float cMask = smoothstep(thick, thick - 0.025, clearDist);
    float xMask = smoothstep(thick, thick - 0.025, xrDist);

    vec3 col = vec3(0.0);
    col += mix(cyan, white, 0.8) * cMask * 1.4;
    col += mix(magenta, white, 0.35) * xMask * 1.5;

    // ---- Divider ----
    float divMask = smoothstep(0.004, 0.0, abs(puv.y + 0.38)) * smoothstep(0.75, 0.3, abs(puv.x));
    col += cyan * 0.35 * divMask;

    // ---- Chevrons ----
    float chL = min(sdSeg2D(puv, vec2(-0.88,0.12), vec2(-0.80,0.0)),
                    sdSeg2D(puv, vec2(-0.80,0.0),  vec2(-0.88,-0.12)));
    float chR = min(sdSeg2D(puv, vec2(0.88,0.12), vec2(0.80,0.0)),
                    sdSeg2D(puv, vec2(0.80,0.0),  vec2(0.88,-0.12)));
    col += magenta * 0.6 * smoothstep(0.008, 0.0, min(chL, chR));

    // ---- Scanlines + flicker inside letters only ----
    float letterMask = max(cMask, xMask);
    float scan = 0.82 + 0.18 * sin(puv.y * 90.0 + time * 2.5);
    col *= mix(1.0, scan, letterMask);
    float flicker = 0.97 + 0.03 * sin(time * 17.0);
    col *= mix(1.0, flicker, letterMask);

    return col;
}

// ============================================================
// Scene SDF
// ============================================================

struct Hit { float dist; int mat; };

Hit sceneMap(vec3 p) {
    float t = pc.cam_pos.w; // time

    Hit best = Hit(1e9, 0);

    // Crystal A – slow orbit, warm colour
    vec3 pa = p - vec3(3.0 * cos(t * 0.23), 1.2 + 0.4 * sin(t * 0.51), -5.0 + sin(t * 0.17));
    float ca = t * 0.4, sa = sin(ca); ca = cos(ca);
    pa.xz = mat2(ca, sa, -sa, ca) * pa.xz;
    float da = sdOctahedron(pa, 0.55);
    if (da < best.dist) { best.dist = da; best.mat = 1; }

    // Crystal B – faster, cool colour
    vec3 pb = p - vec3(-2.8 + 0.6 * sin(t * 0.38), 0.7 + 0.3 * cos(t * 0.72), -4.0);
    float cb = t * -0.6, sb = sin(cb); cb = cos(cb);
    pb.xz = mat2(cb, sb, -sb, cb) * pb.xz;
    float db = sdOctahedron(pb, 0.35);
    if (db < best.dist) { best.dist = db; best.mat = 2; }

    // Crystal C – large, slow, violet
    vec3 pc3 = p - vec3(0.3, 2.5 + 0.6 * sin(t * 0.31), -7.0);
    float cc = t * 0.25, sc = sin(cc); cc = cos(cc);
    pc3.xz = mat2(cc, sc, -sc, cc) * pc3.xz;
    float dc = sdOctahedron(pc3, 0.9);
    if (dc < best.dist) { best.dist = dc; best.mat = 3; }

    // (Logo panel is rendered as a transparent overlay, not part of the SDF scene.)

    // Ground plane
    float dg = p.y + 2.0;
    if (dg < best.dist) { best.dist = dg; best.mat = 4; }

    return best;
}

vec3 calcNormal(vec3 p) {
    const float e = 0.0007;
    return normalize(vec3(
        sceneMap(p + vec3(e, 0, 0)).dist - sceneMap(p - vec3(e, 0, 0)).dist,
        sceneMap(p + vec3(0, e, 0)).dist - sceneMap(p - vec3(0, e, 0)).dist,
        sceneMap(p + vec3(0, 0, e)).dist - sceneMap(p - vec3(0, 0, e)).dist));
}

// ============================================================
// Star field
// ============================================================

float starField(vec3 dir) {
    float stars = 0.0;
    // Dense star/particle layers — Beat Saber floaty particle vibe
    for (int layer = 0; layer < 12; layer++) {
        float scale = 3.0 + float(layer) * 2.5;
        vec3 d = normalize(dir) * scale;
        vec3 cell = floor(d);
        vec3 f    = fract(d);
        vec3 sp   = hash33(cell + float(layer) * 17.3) * 0.6 + 0.2;
        float bri = hash12(cell.xy + float(layer) * 5.1);
        // Vary size: some large soft particles, many tiny sharp ones
        float sz  = mix(0.025, 0.008, float(layer) / 11.0);
        stars += bri * smoothstep(sz, sz * 0.1, length(f - sp));
    }
    return stars;
}

// ============================================================
// Main
// ============================================================

void main() {
    float time = pc.cam_pos.w;
    vec2 uv = frag_uv * 2.0 - 1.0;
    uv.y = -uv.y; // Vulkan Y-flip

    // ---- Ray from camera basis (asymmetric FOV) ----
    vec3 ro = pc.cam_pos.xyz;

    // Interpolate across the full asymmetric frustum:
    //   uv.x = -1 → tan(angle_left),   uv.x = +1 → tan(angle_right)
    //   uv.y = -1 → tan(angle_down),   uv.y = +1 → tan(angle_up)
    float tanH = mix(pc.fov.x, pc.fov.y, (uv.x + 1.0) * 0.5);
    float tanV = mix(pc.fov.z, pc.fov.w, (uv.y + 1.0) * 0.5);

    vec3 rd = normalize(
        pc.cam_fwd.xyz +
        tanH * pc.cam_right.xyz +
        tanV * pc.cam_up.xyz);

    // ---- Ray march ----
    const int   MAX_STEPS = 96;
    const float MAX_DIST  = 40.0;
    const float SURF_EPS  = 0.001;

    // Depth constants (must match near_z/far_z in CompositionLayerDepthInfoKHR)
    const float NEAR_Z = 0.01;
    const float FAR_Z  = 100.0;
    float final_depth  = FAR_Z; // linear depth of closest hit

    float t_ray = 0.0;
    Hit hit = Hit(0.0, 0);
    bool didHit = false;

    for (int i = 0; i < MAX_STEPS; i++) {
        vec3 p = ro + rd * t_ray;
        Hit h  = sceneMap(p);
        if (h.dist < SURF_EPS) { hit = h; didHit = true; break; }
        t_ray += h.dist * 0.85;
        if (t_ray > MAX_DIST) break;
    }

    // ---- Background: near-black with subtle blue hint ----
    float nebula = fbm(rd * 1.8 + vec3(time * 0.01));
    vec3 nebulaCol = mix(vec3(0.002, 0.003, 0.01), vec3(0.008, 0.01, 0.03), nebula);
    float starsVal = starField(rd);
    vec3 bg = nebulaCol + vec3(starsVal);

    vec3 col = bg;

    if (didHit) {
        final_depth = t_ray;
        vec3 pos = ro + rd * t_ray;

        {
            // ---- Normal material shading ----
            vec3 norm = calcNormal(pos);

            vec3 L1 = normalize(vec3(0.6, 1.0, 0.4));
            vec3 L2 = normalize(vec3(-1.0, 0.3, -0.5));
            float diff1 = max(dot(norm, L1), 0.0);
            float diff2 = max(dot(norm, L2), 0.0) * 0.4;

            vec3 H1 = normalize(L1 - rd);
            float spec = pow(max(dot(norm, H1), 0.0), 64.0);
            float rim  = pow(1.0 - max(dot(norm, -rd), 0.0), 3.0);

            vec3 matCol;
            if (hit.mat == 1)      matCol = vec3(1.0, 0.55, 0.15);  // amber
            else if (hit.mat == 2) matCol = vec3(0.2, 0.8, 1.0);    // cyan
            else if (hit.mat == 3) matCol = vec3(0.75, 0.3, 1.0);   // violet
            else {
                // Ground grid – fade out with distance to avoid shimmer/aliasing
                float distFade = 1.0 - smoothstep(3.0, 12.0, t_ray);
                vec2 gridAbs = abs(fract(pos.xz) - 0.5);
                // Widen the smoothstep with distance for anti-aliasing
                float fw = max(0.02, t_ray * 0.008);
                float line = min(gridAbs.x, gridAbs.y);
                float gridVal = (1.0 - smoothstep(0.02, 0.02 + fw, line)) * distFade;
                matCol = mix(vec3(0.05, 0.07, 0.12), vec3(0.2, 0.4, 0.7), gridVal * 0.5);
            }

            col = matCol * (diff1 + diff2 + 0.05)
                + vec3(1.0) * spec * 0.9
                + matCol * rim * 0.4;

            vec3 refl = reflect(rd, norm);
            col += matCol * (starField(refl) + fbm(refl * 2.0) * 0.1) * 0.35;

            float fog = 1.0 - exp(-t_ray * 0.04);
            col = mix(col, bg, fog);
        }
    }

    // ---- Logo panel: transparent holographic overlay ----
    // Analytic ray-plane intersection at z = PANEL_POS.z, then additive blend.
    if (abs(rd.z) > 0.0001) {
        float tPanel = (PANEL_POS.z - ro.z) / rd.z;
        if (tPanel > 0.0) {
            vec3 hitP = ro + rd * tPanel;
            vec2 panelUV = vec2(
                (hitP.x - PANEL_POS.x) / PANEL_HALF.x,
                (hitP.y - PANEL_POS.y) / PANEL_HALF.y
            );
            // Only shade if inside the panel rectangle
            if (abs(panelUV.x) < 1.0 && abs(panelUV.y) < 1.0) {
                vec3 logoCol = renderLogo(panelUV, time);
                logoCol *= step(0.03, dot(logoCol, vec3(0.333)));
                col += logoCol;
                // Logo panel is closer than scene behind it
                if (tPanel < final_depth) final_depth = tPanel;
            }
        }
    }

    // ---- Hand skeleton rendering (analytic intersections) ----
    {
        // Bone connections: pairs of (start, end) joint indices within one hand.
        // palm=0, wrist=1, thumb: 2-5, index: 6-10, middle: 11-15, ring: 16-20, little: 21-25
        const int BONE_A[25] = int[25](
            0,                         // palm -> wrist
            1, 2, 3, 4,               // thumb chain
            1, 6, 7, 8, 9,            // index chain
            1, 11, 12, 13, 14,        // middle chain
            1, 16, 17, 18, 19,        // ring chain
            1, 21, 22, 23, 24         // little chain
        );
        const int BONE_B[25] = int[25](
            1,
            2, 3, 4, 5,
            6, 7, 8, 9, 10,
            11, 12, 13, 14, 15,
            16, 17, 18, 19, 20,
            21, 22, 23, 24, 25
        );

        float handT = 1e9;
        vec3 handNorm = vec3(0.0);
        vec3 handColor = vec3(0.0);

        for (int h = 0; h < 2; h++) {
            if ((h == 0 && hands.hand_active.x < 0.5) || (h == 1 && hands.hand_active.y < 0.5))
                continue;

            int base = h * 26;
            vec3 jointColor = h == 0 ? vec3(0.0, 0.85, 1.0) : vec3(1.0, 0.05, 0.55);

            // Test joint spheres
            for (int j = 0; j < 26; j++) {
                vec4 jd = hands.joints[base + j];
                float r = max(jd.w, 0.005) * 1.5;
                float t = raySphere(ro, rd, jd.xyz, r);
                if (t > 0.0 && t < handT) {
                    handT = t;
                    vec3 hitP = ro + rd * t;
                    handNorm = normalize(hitP - jd.xyz);
                    handColor = jointColor;
                }
            }

            // Test bone capsules
            float boneRadius = 0.004;
            for (int b = 0; b < 25; b++) {
                vec4 ja = hands.joints[base + BONE_A[b]];
                vec4 jb = hands.joints[base + BONE_B[b]];
                float t = rayCapsule(ro, rd, ja.xyz, jb.xyz, boneRadius);
                if (t > 0.0 && t < handT) {
                    handT = t;
                    vec3 hitP = ro + rd * t;
                    vec3 ab = jb.xyz - ja.xyz;
                    float proj = clamp(dot(hitP - ja.xyz, ab) / max(dot(ab, ab), 0.0001), 0.0, 1.0);
                    vec3 closest = ja.xyz + ab * proj;
                    handNorm = normalize(hitP - closest);
                    handColor = jointColor * 0.7;
                }
            }
        }

        // ---- Controller rendering (if controllers active, replaces hands) ----
        for (int c = 0; c < 2; c++) {
            float isActive = c == 0 ? hands.hand_active.z : hands.hand_active.w;
            if (isActive < 0.5) continue;

            vec3 gripPos = hands.ctrl_grip[c].xyz;
            float gripRadius = max(hands.ctrl_grip[c].w, 0.015);
            vec3 aimPos = hands.ctrl_aim_pos[c].xyz;
            vec3 aimDir = normalize(hands.ctrl_aim_dir[c].xyz);
            vec3 gripRight = hands.ctrl_grip_right[c].xyz;
            vec3 gripUp = hands.ctrl_grip_up[c].xyz;
            vec3 ctrlColor = c == 0 ? vec3(0.0, 0.85, 1.0) : vec3(1.0, 0.05, 0.55);

            // Read input state
            float trigger = hands.ctrl_inputs[c].x;
            float squeeze = hands.ctrl_inputs[c].y;
            vec2 stick = hands.ctrl_inputs[c].zw;
            float btn1Touch = hands.ctrl_buttons[c].x;  // X or A touch
            float btn2Touch = hands.ctrl_buttons[c].y;  // Y or B touch
            float stickClick = hands.ctrl_buttons[c].z;
            float menuBtn = hands.ctrl_buttons[c].w;
            float btn1Click = hands.ctrl_clicks[c].x;   // X or A click
            float btn2Click = hands.ctrl_clicks[c].y;   // Y or B click
            float triggerTouch = hands.ctrl_touches[c].x;
            float squeezeTouch = hands.ctrl_touches[c].y;
            float stickTouch = hands.ctrl_touches[c].z;

            // Unified 3-state colors for all interactive elements:
            //   idle    = dim controller tint
            //   touched = soft amber
            //   pressed = bright white-gold
            vec3 stIdle    = ctrlColor * 0.35;
            vec3 stTouched = vec3(1.0, 0.65, 0.2);
            vec3 stPressed = vec3(1.0, 0.95, 0.7);

            // Controller body: capsule from grip along aim direction
            float bodyRadius = gripRadius * 0.7 * (1.0 + squeeze * 0.3);
            vec3 ctrlFwd = gripPos + aimDir * 0.10;
            float tBody = rayCapsule(ro, rd, gripPos, ctrlFwd, bodyRadius);
            if (tBody > 0.0 && tBody < handT) {
                handT = tBody;
                vec3 hitP = ro + rd * tBody;
                vec3 ab = ctrlFwd - gripPos;
                float proj = clamp(dot(hitP - gripPos, ab) / max(dot(ab, ab), 0.0001), 0.0, 1.0);
                handNorm = normalize(hitP - (gripPos + ab * proj));
                handColor = ctrlColor * (1.0 + squeeze * 0.5);
            }

            // Grip sphere at base
            float tGrip = raySphere(ro, rd, gripPos, gripRadius);
            if (tGrip > 0.0 && tGrip < handT) {
                handT = tGrip;
                handNorm = normalize(ro + rd * tGrip - gripPos);
                // Squeezed trumps touched trumps idle
                handColor = squeeze > 0.2 ? mix(stTouched, stPressed, squeeze)
                          : squeezeTouch > 0.5 ? stTouched
                          : stIdle;
            }

            // Trigger: small sphere below the front
            vec3 triggerPos = gripPos + aimDir * 0.06 - gripUp * 0.015;
            triggerPos += aimDir * (-0.01 * trigger);
            float tTrig = raySphere(ro, rd, triggerPos, 0.008);
            if (tTrig > 0.0 && tTrig < handT) {
                handT = tTrig;
                handNorm = normalize(ro + rd * tTrig - triggerPos);
                // Pulled trumps touched trumps idle
                handColor = trigger > 0.2 ? mix(stTouched, stPressed, trigger)
                          : triggerTouch > 0.5 ? stTouched
                          : stIdle;
            }

            // Thumbstick: sphere on top, offset by stick x/y
            vec3 stickBase = gripPos + aimDir * 0.02 + gripUp * 0.018;
            vec3 stickPos = stickBase + gripRight * stick.x * 0.006 + aimDir * stick.y * 0.006;
            // Sink slightly when clicked instead of changing radius (avoids flicker)
            if (stickClick > 0.5) stickPos -= gripUp * 0.002;
            float tStick = raySphere(ro, rd, stickPos, 0.006);
            if (tStick > 0.0 && tStick < handT) {
                handT = tStick;
                handNorm = normalize(ro + rd * tStick - stickPos);
                // Pressed trumps touched trumps idle
                handColor = stickClick > 0.5 ? stPressed
                          : stickTouch > 0.5 ? stTouched
                          : stIdle;
            }

            // Buttons (A/B or X/Y): two spheres on top
            vec3 btn1Pos = gripPos + aimDir * 0.04 + gripUp * 0.016 + gripRight * 0.008;
            vec3 btn2Pos = gripPos + aimDir * 0.04 + gripUp * 0.016 - gripRight * 0.008;
            float tBtn1 = raySphere(ro, rd, btn1Pos, 0.005);
            if (tBtn1 > 0.0 && tBtn1 < handT) {
                handT = tBtn1;
                handNorm = normalize(ro + rd * tBtn1 - btn1Pos);
                handColor = btn1Click > 0.5 ? stPressed
                          : btn1Touch > 0.5 ? stTouched
                          : stIdle;
            }
            float tBtn2 = raySphere(ro, rd, btn2Pos, 0.005);
            if (tBtn2 > 0.0 && tBtn2 < handT) {
                handT = tBtn2;
                handNorm = normalize(ro + rd * tBtn2 - btn2Pos);
                handColor = btn2Click > 0.5 ? stPressed
                          : btn2Touch > 0.5 ? stTouched
                          : stIdle;
            }

            // Menu button: small sphere (both controllers for emulated PS Sense)
            vec3 menuPos = gripPos + aimDir * 0.06 + gripUp * 0.016;
            float tMenu = raySphere(ro, rd, menuPos, 0.004);
            if (tMenu > 0.0 && tMenu < handT) {
                handT = tMenu;
                handNorm = normalize(ro + rd * tMenu - menuPos);
                handColor = menuBtn > 0.5 ? stPressed : stIdle;
            }

            // Pointer ray: thin beam from aim pose (w = max length, 0 = default 3m)
            float rayLen = hands.ctrl_aim_dir[c].w > 0.0 ? hands.ctrl_aim_dir[c].w : 3.0;
            vec3 rayEnd = aimPos + aimDir * rayLen;
            float tBeam = rayCapsule(ro, rd, aimPos, rayEnd, 0.0015);
            if (tBeam > 0.0 && tBeam < handT) {
                handT = tBeam;
                vec3 hitP = ro + rd * tBeam;
                vec3 ab = rayEnd - aimPos;
                float proj = clamp(dot(hitP - aimPos, ab) / max(dot(ab, ab), 0.0001), 0.0, 1.0);
                handNorm = normalize(hitP - (aimPos + ab * proj));
                // Fade the ray color along its length
                handColor = ctrlColor * (1.0 - proj * 0.7);
            }

            // Dot indicator is drawn in the panel fragment shader (on top of the panel surface).
        }

        // Composite hand/controller on top of scene if it's closer
        if (handT < 1e8) {
            if (!didHit || handT < t_ray) {
                vec3 L = normalize(vec3(0.6, 1.0, 0.4));
                float diff = max(dot(handNorm, L), 0.0);
                float rim = pow(1.0 - max(dot(handNorm, -rd), 0.0), 2.0);
                col = handColor * (diff * 0.8 + 0.2) + handColor * rim * 0.5;
                if (handT < final_depth) final_depth = handT;
            }
        }
    }

    // ---- Tone map (ACES filmic) ----
    col = col * (col + 0.0245786) / (col * (0.983729 * col + 0.432951) + 0.238081);

    // ---- Vignette (peripheral views only) ----
    // Views 0,1 = peripheral; views 2,3 = foveal inset.
    // Applying vignette to foveal insets creates a visible dark border.
    if (pc.cam_right.w < 1.5) {
        col *= 1.0 - 0.4 * dot(uv * 0.7, uv * 0.7);
    }

    out_color = vec4(col, 1.0);

    // Write depth for XR_KHR_composition_layer_depth (better reprojection).
    // Standard Vulkan depth [0,1]: 0 = near, 1 = far.
    float d = clamp(final_depth, NEAR_Z, FAR_Z);
    gl_FragDepth = (FAR_Z * (d - NEAR_Z)) / (d * (FAR_Z - NEAR_Z));
}
