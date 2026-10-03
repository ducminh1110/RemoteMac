//! NV12 -> RGB on the GPU with a pixel shader, the way Moonlight's D3D11 renderer draws decoded
//! frames (moonlight-qt d3d11va.cpp + d3d11_yuv420_pixel.hlsl): the NV12 texture gets two
//! shader views (R8 for luma, R8G8 for the half-size chroma plane) and a full-window triangle
//! samples both and applies the BT.709 limited-range matrix. Unlike the D3D11 video processor
//! (which drew nothing on some drivers) this is plain 3D work every GPU does the same way.
//!
//! [`self_test`] draws a known NV12 picture once and reads it back: the viewer only shows GPU
//! pictures directly when the colours come out right, else it copies them to system memory.

use std::sync::OnceLock;
use windows::core::{s, PCSTR};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_SRV_DIMENSION_TEXTURE2D, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

const HLSL: &str = r#"
Texture2D<float> lumaPlane : register(t0);
Texture2D<float2> chromaPlane : register(t1);
SamplerState samp : register(s0);
cbuffer Frame : register(b0) { float2 texScale; float2 pad; };
struct V { float4 pos : SV_POSITION; float2 tex : TEXCOORD0; };

// one triangle covering the target; texcoords reach texScale at the far edges of the picture
V vs_main(uint id : SV_VertexID) {
    V o;
    float2 t = float2((id << 1) & 2, id & 2);
    o.pos = float4(t * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    o.tex = t * texScale;
    return o;
}

// BT.709, limited range (16-235 / 16-240), as the Mac encodes
float4 ps_main(V i) : SV_TARGET {
    float y = (lumaPlane.Sample(samp, i.tex) - 16.0 / 255.0) * (255.0 / 219.0);
    float2 c = (chromaPlane.Sample(samp, i.tex) - 128.0 / 255.0) * (255.0 / 224.0);
    float3 rgb = float3(y + 1.5748 * c.y, y - 0.1873 * c.x - 0.4681 * c.y, y + 1.8556 * c.x);
    return float4(saturate(rgb), 1.0);
}
"#;

pub struct Nv12Renderer {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    cbuf: ID3D11Buffer,
}

// Used from the UI thread only; the device is multithread protected.
unsafe impl Send for Nv12Renderer {}
unsafe impl Sync for Nv12Renderer {}

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob, String> {
    unsafe {
        let (mut code, mut errs) = (None, None);
        let r = D3DCompile(HLSL.as_ptr() as *const _, HLSL.len(), s!("nv12.hlsl"), None, None, entry, target, 0, 0, &mut code, Some(&mut errs));
        if let Err(e) = r {
            let msg = errs.map(|b| String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())).into_owned()).unwrap_or_default();
            return Err(format!("shader: {e} {msg}"));
        }
        code.ok_or_else(|| "shader: no code".into())
    }
}

fn bytes(b: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) }
}

impl Nv12Renderer {
    pub fn new(device: &ID3D11Device) -> Result<Self, String> {
        let vsb = compile(s!("vs_main"), s!("vs_4_0"))?;
        let psb = compile(s!("ps_main"), s!("ps_4_0"))?;
        unsafe {
            let (mut vs, mut ps, mut sampler, mut cbuf) = (None, None, None, None);
            device.CreateVertexShader(bytes(&vsb), None, Some(&mut vs)).map_err(|e| format!("vertex shader: {e}"))?;
            device.CreatePixelShader(bytes(&psb), None, Some(&mut ps)).map_err(|e| format!("pixel shader: {e}"))?;
            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            device.CreateSamplerState(&sd, Some(&mut sampler)).map_err(|e| format!("sampler: {e}"))?;
            let bd = D3D11_BUFFER_DESC { ByteWidth: 16, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
            device.CreateBuffer(&bd, None, Some(&mut cbuf)).map_err(|e| format!("constant buffer: {e}"))?;
            Ok(Self { vs: vs.ok_or("vertex shader")?, ps: ps.ok_or("pixel shader")?, sampler: sampler.ok_or("sampler")?, cbuf: cbuf.ok_or("constant buffer")? })
        }
    }

    /// Draw the visible `w`x`h` of NV12 texture `src` over the whole of `target` (a BGRA or
    /// RGBA render target `out_w`x`out_h`).
    pub fn draw(&self, device: &ID3D11Device, ctx: &ID3D11DeviceContext, src: &ID3D11Texture2D, (w, h): (u32, u32), target: &ID3D11Texture2D, (out_w, out_h): (u32, u32)) -> Result<(), &'static str> {
        unsafe {
            let mut td = D3D11_TEXTURE2D_DESC::default();
            src.GetDesc(&mut td);
            let view = |fmt: DXGI_FORMAT| -> Result<ID3D11ShaderResourceView, &'static str> {
                let d = D3D11_SHADER_RESOURCE_VIEW_DESC {
                    Format: fmt,
                    ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 } },
                };
                let mut v = None;
                device.CreateShaderResourceView(src, Some(&d), Some(&mut v)).map_err(|_| "NV12 shader view")?;
                v.ok_or("NV12 shader view")
            };
            let (luma, chroma) = (view(DXGI_FORMAT_R8_UNORM)?, view(DXGI_FORMAT_R8G8_UNORM)?);
            let mut rtv = None;
            device.CreateRenderTargetView(target, None, Some(&mut rtv)).map_err(|_| "render target")?;
            let rtv = rtv.ok_or("render target")?;
            let scale = [w as f32 / td.Width.max(1) as f32, h as f32 / td.Height.max(1) as f32, 0.0, 0.0];
            ctx.UpdateSubresource(&self.cbuf, 0, None, scale.as_ptr() as *const _, 0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(rtv)]), None);
            let vp = D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: out_w as f32, Height: out_h as f32, MinDepth: 0.0, MaxDepth: 1.0 };
            ctx.RSSetViewports(Some(&[vp]));
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.cbuf.clone())]));
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetShaderResources(0, Some(&[Some(luma), Some(chroma)]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.Draw(3, 0);
            // unbind, so the texture can be written again
            ctx.PSSetShaderResources(0, Some(&[None, None]));
            ctx.OMSetRenderTargets(None, None);
            Ok(())
        }
    }
}

/// The renderer for the shared device, when its self-test passed.
pub fn shared() -> Option<&'static Nv12Renderer> {
    static R: OnceLock<Option<Nv12Renderer>> = OnceLock::new();
    R.get_or_init(|| {
        let g = crate::gpu::shared()?;
        match Nv12Renderer::new(&g.device).and_then(|r| self_test(&g.device, &g.ctx, &r).map(|()| r)) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("GPU colour conversion unavailable ({e}); decoded pictures are copied to memory");
                None
            }
        }
    })
    .as_ref()
}

/// Draw a 16x16 NV12 picture (left half red, right half blue, in BT.709 limited range) and
/// check the colours that come out.
pub fn self_test(device: &ID3D11Device, ctx: &ID3D11DeviceContext, r: &Nv12Renderer) -> Result<(), String> {
    const N: u32 = 16;
    // red (255,0,0) and blue (0,0,255) in BT.709 limited range: Y, Cb, Cr
    let (red, blue) = ([63u8, 102, 240], [32u8, 240, 118]);
    let mut nv12 = vec![0u8; (N * N * 3 / 2) as usize];
    for y in 0..N {
        for x in 0..N {
            nv12[(y * N + x) as usize] = if x < N / 2 { red[0] } else { blue[0] };
        }
    }
    for y in 0..N / 2 {
        for x in 0..N / 2 {
            let c = if x < N / 4 { red } else { blue };
            let o = (N * N + y * N + x * 2) as usize;
            nv12[o] = c[1];
            nv12[o + 1] = c[2];
        }
    }
    unsafe {
        let mk = |fmt: DXGI_FORMAT, bind: u32, usage: D3D11_USAGE, cpu: u32, init: Option<&D3D11_SUBRESOURCE_DATA>| -> Result<ID3D11Texture2D, String> {
            let d = D3D11_TEXTURE2D_DESC {
                Width: N,
                Height: N,
                MipLevels: 1,
                ArraySize: 1,
                Format: fmt,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: usage,
                BindFlags: bind,
                CPUAccessFlags: cpu,
                MiscFlags: 0,
            };
            let mut t = None;
            device.CreateTexture2D(&d, init.map(|i| i as *const _), Some(&mut t)).map_err(|e| format!("texture {fmt:?}: {e}"))?;
            t.ok_or_else(|| "texture".into())
        };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: nv12.as_ptr() as *const _, SysMemPitch: N, SysMemSlicePitch: 0 };
        let src = mk(DXGI_FORMAT_NV12, D3D11_BIND_SHADER_RESOURCE.0 as u32, D3D11_USAGE_DEFAULT, 0, Some(&init))?;
        let target = mk(DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_RENDER_TARGET.0 as u32, D3D11_USAGE_DEFAULT, 0, None)?;
        let staging = mk(DXGI_FORMAT_B8G8R8A8_UNORM, 0, D3D11_USAGE_STAGING, D3D11_CPU_ACCESS_READ.0 as u32, None)?;
        r.draw(device, ctx, &src, (N, N), &target, (N, N)).map_err(|e| e.to_string())?;
        ctx.CopyResource(&staging, &target);
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m)).map_err(|e| format!("read back: {e}"))?;
        let px = |x: u32, y: u32| {
            let p = (m.pData as *const u8).add((y * m.RowPitch + x * 4) as usize);
            [*p.add(2), *p.add(1), *p] // BGRA -> RGB
        };
        let (l, rt) = (px(2, 8), px(13, 8));
        ctx.Unmap(&staging, 0);
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| (*x as i32 - y as i32).abs() <= 24);
        if near(l, [255, 0, 0]) && near(rt, [0, 0, 255]) {
            Ok(())
        } else {
            Err(format!("wrong colours: {l:?} {rt:?}"))
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn colours_come_out_right() {
        // the shared device (WARP on a machine without a GPU): shaders compile, NV12 views work
        let g = crate::gpu::shared().expect("a D3D11 device");
        let r = super::Nv12Renderer::new(&g.device).unwrap();
        super::self_test(&g.device, &g.ctx, &r).unwrap();
    }
}
