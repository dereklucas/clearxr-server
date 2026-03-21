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
    out_color = vec4(tex.rgb, tex.a * opacity);
}
