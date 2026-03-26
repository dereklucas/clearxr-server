#version 450

layout(location = 0) in vec2 frag_uv;

layout(set = 0, binding = 0) uniform sampler2D panel_tex;

layout(push_constant) uniform PC {
    vec4 cam_pos;
    vec4 cam_right;
    vec4 cam_up;
    vec4 cam_fwd;
    vec4 fov;
    vec4 panel_center;  // w = opacity
    vec4 panel_right;
    vec4 panel_up;
} pc;

layout(location = 0) out vec4 out_color;

void main() {
    vec4 tex = texture(panel_tex, frag_uv);
    float opacity = pc.panel_center.w;
    vec3 col = tex.rgb;
    float a = tex.a * opacity;

    // Pointer dot: draw a small bright circle at the hit UV
    float dot_u = pc.panel_right.w;
    float dot_v = pc.panel_up.w;
    if (dot_u >= 0.0) {
        // Aspect-correct distance in UV space
        float aspect = pc.cam_right.w / max(pc.cam_up.w, 0.001); // width / height
        vec2 delta = vec2((frag_uv.x - dot_u) * aspect, frag_uv.y - dot_v);
        float dist = length(delta);
        float radius = 0.012 * aspect; // dot size in UV
        float ring = 0.003 * aspect;

        // Bright center dot
        float dot_alpha = smoothstep(radius, radius - ring, dist);
        vec3 dot_color = vec3(0.4, 0.9, 1.0); // cyan
        col = mix(col, dot_color, dot_alpha * 0.9);
        a = max(a, dot_alpha * 0.95);
    }

    out_color = vec4(col, a);
}
