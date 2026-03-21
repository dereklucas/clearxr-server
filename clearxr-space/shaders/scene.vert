#version 450

// Fullscreen triangle trick: no vertex buffer needed.
// Three vertices from gl_VertexIndex cover the entire clip-space quad.
layout(location = 0) out vec2 frag_uv; // [0,1] screen UV

void main() {
    // Generates a triangle that covers [-1,1] NDC:
    //   v0 = (-1, -1), v1 = (3, -1), v2 = (-1, 3)
    vec2 uv = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    frag_uv = uv;
    gl_Position = vec4(uv * 2.0 - 1.0, 0.0, 1.0);
}
