struct float4 {
    float x;
    float y;
    float z;
    float w;
};

struct float2 {
    float u;
    float v;
};

struct VertexIn {
    float4 position [[position]];
    float2 uv [[user(locn0)]];
};

float4 tint(float4 color) {
    return color;
}

vertex float4 vertex_main(VertexIn in [[stage_in]]) {
    return tint(in.position);
}
