#version 450

// Renders a textured quad in 3D space.
// 4 vertices form a quad via triangle strip; positions computed from push constants.

layout(push_constant) uniform PC {
    vec4 cam_pos;       // xyz = camera position, w = time
    vec4 cam_right;     // xyz = camera right,    w = panel_width
    vec4 cam_up;        // xyz = camera up,       w = panel_height
    vec4 cam_fwd;       // xyz = camera forward,  w = unused
    vec4 fov;           // same as scene shader
    vec4 panel_center;  // xyz = world position of panel center, w = opacity
    vec4 panel_right;   // xyz = panel local right axis (unit), w = unused
    vec4 panel_up;      // xyz = panel local up axis (unit),    w = unused
} pc;

layout(location = 0) out vec2 frag_uv;

void main() {
    // 4 vertices: triangle strip forming a quad
    //  0 = bottom-left, 1 = bottom-right, 2 = top-left, 3 = top-right
    vec2 offsets[4] = vec2[](
        vec2(-0.5, -0.5),
        vec2( 0.5, -0.5),
        vec2(-0.5,  0.5),
        vec2( 0.5,  0.5)
    );

    vec2 off = offsets[gl_VertexIndex];
    float panel_w = pc.cam_right.w;
    float panel_h = pc.cam_up.w;

    // World position of this vertex
    vec3 world_pos = pc.panel_center.xyz
        + pc.panel_right.xyz * (off.x * panel_w)
        + pc.panel_up.xyz    * (off.y * panel_h);

    // Manual view-projection: project world_pos through the camera
    vec3 to_vert = world_pos - pc.cam_pos.xyz;
    vec3 cam_r = pc.cam_right.xyz;
    vec3 cam_u = pc.cam_up.xyz;
    vec3 cam_f = pc.cam_fwd.xyz;

    float vz = dot(to_vert, cam_f); // depth along camera forward
    float vx = dot(to_vert, cam_r); // horizontal offset
    float vy = dot(to_vert, cam_u); // vertical offset

    // Asymmetric projection using FOV tangents (matches scene shader convention)
    float tan_l = pc.fov.x; // negative
    float tan_r = pc.fov.y; // positive
    float tan_d = pc.fov.z; // negative
    float tan_u = pc.fov.w; // positive

    // NDC: map [tan_l..tan_r] -> [-1..1], [tan_d..tan_u] -> [-1..1]
    float ndc_x = 2.0 * (vx / vz - tan_l) / (tan_r - tan_l) - 1.0;
    float ndc_y = 2.0 * (vy / vz - tan_d) / (tan_u - tan_d) - 1.0;

    // Simple near/far depth mapping
    float near = 0.05;
    float far = 100.0;
    float ndc_z = (far * (vz - near)) / (vz * (far - near));

    // Clip behind camera
    if (vz < near) {
        gl_Position = vec4(0.0, 0.0, -1.0, 1.0); // degenerate
    } else {
        gl_Position = vec4(ndc_x, ndc_y, ndc_z, 1.0);
    }

    // UV: map vertex offset to [0,1] range
    frag_uv = off + 0.5;
}
