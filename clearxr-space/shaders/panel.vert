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

    // View-space: project world_pos into camera space
    vec3 to_vert = world_pos - pc.cam_pos.xyz;
    vec3 cam_r = pc.cam_right.xyz;
    vec3 cam_u = pc.cam_up.xyz;
    vec3 cam_f = pc.cam_fwd.xyz;

    float vx = dot(to_vert, cam_r); // horizontal
    float vy = dot(to_vert, cam_u); // vertical
    float vz = dot(to_vert, cam_f); // depth

    // Asymmetric projection using FOV tangents
    float tan_l = pc.fov.x; // negative
    float tan_r = pc.fov.y; // positive
    float tan_d = pc.fov.z; // negative
    float tan_u = pc.fov.w; // positive

    // Build clip-space coordinates (let GPU do perspective divide via w)
    // Maps view-space to clip-space: x_clip = (2*vx - vz*(tan_l+tan_r)) / (tan_r - tan_l)
    float clip_x = (2.0 * vx - vz * (tan_l + tan_r)) / (tan_r - tan_l);
    // Negate Y: Vulkan NDC has Y pointing down, our view-space has Y pointing up
    float clip_y = -(2.0 * vy - vz * (tan_d + tan_u)) / (tan_u - tan_d);

    // Depth: standard perspective depth mapping
    float near = 0.05;
    float far = 100.0;
    float clip_z = far * (vz - near) / (far - near);

    // w = vz: GPU divides by w for perspective-correct interpolation
    gl_Position = vec4(clip_x, clip_y, clip_z, vz);

    // UV: map vertex offset to [0,1] range
    // Flip V so texture top (row 0) maps to panel top (+up direction)
    frag_uv = vec2(off.x + 0.5, 0.5 - off.y);
}
