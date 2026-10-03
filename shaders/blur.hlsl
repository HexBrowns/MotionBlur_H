// MotionBlur_H のぼかし。原作 ObjectMotionBlur_LK v2.0.2 の src/object/intern/shaders/blur.hlsl を写したもの。
//
// 原作との違い: 原作は CPU で各サンプルの変換（trajectory）を作り StructuredBuffer で渡していた。
// 本体の exec_pixelshader_data は定数バッファしか渡せないので、段ごとの始点と変化量を渡し、
// 変換はここで作る（原作の CreateTrajectory と同じ式。回転は t の位置の角度を直接 cos / sin する）。
//
// t0 = 元の画像、t1 = 色付けのグラデーションマップ。s0 = 本体の clip サンプラー（線形補間・範囲外は透明。原作の BORDER と同じ）

#define MAX_LINKS 16

static const float kEpsilon = 1.0e-5;
static const float3 kBT709 = float3(0.2126, 0.7152, 0.0722);

Texture2D target_image : register(t0);
Texture2D map : register(t1);
SamplerState linear_sampler : register(s0);

cbuffer params : register(b0) {
    float4 tr0;            // object.transform の 1 行目（xyz）
    float4 tr1;            // 2 行目
    float4 origin_texel;   // origin.xy, texel.xy
    float4 mix_falloff;    // mix.xy, falloff.xy
    float4 misc;           // samples, map_inset, alpha_mode, seed
    float4 pivot;          // origin.xy, extent.xy
    float4 counts;         // x = 段の数
    float4 link_pos[MAX_LINKS];   // origin.xy, extent.xy
    float4 link_cmp[MAX_LINKS];   // origin.xy, extent.xy
    float4 link_rot[MAX_LINKS];   // origin, extent
}

/*
The following function is a modified version of pcg4d function
Original implementation by Mark Jarzynski & Marc Olano
https://github.com/markjarzynski/PCG3D/blob/master/LICENSE
*/
uint4 pcg4d(uint4 v) {
    v = v * 1664525u + 1013904223u;

    v.x += v.y * v.w;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    v.w += v.y * v.z;

    v = v ^ v >> 16u;

    v.x += v.y * v.w;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    v.w += v.y * v.z;

    return v;
}

float hash(float2 p, float2 s) {
    return dot(pcg4d(uint4(p, s)), 1u) / 4294967295.0;
}

float4 sample_image(float2 pos) {
    return target_image.Sample(linear_sampler, pos * origin_texel.zw);
}

float4 tint(float4 color, float t) {
    const float map_inset = misc.y;
    const float y = saturate(dot(color.rgb, kBT709) * rcp(max(color.a, kEpsilon)));
    const float4 src = map.Sample(linear_sampler, float2(lerp(map_inset, 1.0 - map_inset, y), t));
    color.rgb = mad(color.rgb, 1.0 - src.a, src.rgb * color.a);
    return color;
}

float4 main(float4 pos : SV_Position) : SV_Target {
    const int samples = (int)misc.x;
    const int link_count = (int)counts.x;
    const float dither = hash(pos.xy, misc.ww);

    const float3 p = float3(pos.xy + origin_texel.xy, 1.0);
    const float4 dry = sample_image(p.xy) * mix_falloff.x;
    const float3 q = float3(dot(tr0.xyz, p), dot(tr1.xyz, p), 1.0);

    float4 wet = float4(0.0, 0.0, 0.0, 0.0);
    float norm = 0.0;

    [loop]
    for (int i = 0; i < samples; ++i) {
        const float t = (float(i) + 0.5) * rcp(float(samples));

        // node = 原作 CreateTrajectory の 1 サンプル分
        float2 n0 = float2(1.0, 0.0);   // 線形部の 1 行目
        float2 n1 = float2(0.0, 1.0);   // 2 行目
        float2 nt = float2(0.0, 0.0);   // 平行移動

        [loop]
        for (int j = 0; j < link_count; ++j) {
            const float2 lp = link_pos[j].xy + link_pos[j].zw * t;
            const float2 cmp = link_cmp[j].xy + link_cmp[j].zw * t;
            const float ang = link_rot[j].x + link_rot[j].y * t;
            float s = 0.0;
            float c = 1.0;
            sincos(ang, s, c);

            // L = [[cmp.x*c, cmp.x*s], [-cmp.y*s, cmp.y*c]]
            const float2 l0 = float2(cmp.x * c, cmp.x * s);
            const float2 l1 = float2(-cmp.y * s, cmp.y * c);

            const float2 d = nt - lp;
            nt = float2(dot(l0, d), dot(l1, d));

            const float2 m0 = float2(l0.x * n0.x + l0.y * n1.x, l0.x * n0.y + l0.y * n1.y);
            const float2 m1 = float2(l1.x * n0.x + l1.y * n1.x, l1.x * n0.y + l1.y * n1.y);
            n0 = m0;
            n1 = m1;
        }
        nt += pivot.xy + pivot.zw * t;

        const float2 uv = float2(dot(float3(n0, nt.x), q), dot(float3(n1, nt.y), q));
        const float weight = smoothstep(0.0, mix_falloff.z, t) * smoothstep(0.0, mix_falloff.w, 1.0 - t);

        wet += tint(sample_image(uv), t) * weight;
        norm += weight;
    }

    wet *= rcp(max(norm, kEpsilon));

    const float4 dissolved = wet * rcp(max(wet.a, kEpsilon)) * step(dither + kEpsilon, wet.a);

    return mad(1.0 - dry.a, lerp(wet, dissolved, misc.z) * mix_falloff.y, dry);
}
