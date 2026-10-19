mod field;
mod glsl;
mod layout;

use proc_macro::TokenStream as CompilerTokenStream;

use quote::quote;
use syn::spanned::Spanned;
use syn::{parse_macro_input, DeriveInput, ItemStruct};

const STD140: layout::Options = layout::Options {
    trait_name: "Std140",
    mod_name: "std140",
    min_struct_alignment: 16,
    sort: false,
    pad: layout::Pad::Array,
};

const STD430: layout::Options = layout::Options {
    trait_name: "Std430",
    mod_name: "std430",
    min_struct_alignment: 0,
    sort: false,
    pad: layout::Pad::Array,
};

fn expand_derive(input: CompilerTokenStream, options: layout::Options) -> CompilerTokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    layout::emit(input, options)
        .unwrap_or_else(|error| error.to_compile_error())
        .into()
}

fn expand_attribute(
    args: CompilerTokenStream,
    item: CompilerTokenStream,
    options: layout::Options,
) -> CompilerTokenStream {
    let mut sort = false;
    let mut pad_mode: Option<(PadMode, proc_macro2::Span)> = None;
    let mut pad_type: Option<(layout::ScalarPad, proc_macro2::Span)> = None;
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("sort") {
            if sort {
                return Err(meta.error("duplicate `sort` option"));
            }
            sort = true;
            Ok(())
        } else if meta.path.is_ident("pad") {
            if pad_mode.is_some() {
                return Err(meta.error("duplicate `pad` option"));
            }
            pad_mode = Some((parse_pad_mode(&meta)?, meta.path.span()));
            Ok(())
        } else if meta.path.is_ident("pad_type") {
            if pad_type.is_some() {
                return Err(meta.error("duplicate `pad_type` option"));
            }
            pad_type = Some((parse_pad_type(&meta)?, meta.path.span()));
            Ok(())
        } else {
            Err(meta.error("unsupported option, expected `sort`, `pad` or `pad_type`"))
        }
    });
    parse_macro_input!(args with parser);

    let pad = match (pad_mode, pad_type) {
        (None, None) | (Some((PadMode::Array, _)), None) => layout::Pad::Array,
        (Some((PadMode::Array, _)), Some((_, span))) => {
            return syn::Error::new(span, "`pad_type` is only used with `pad = scalar`")
                .to_compile_error()
                .into()
        }
        (None, Some((_, span))) => {
            return syn::Error::new(span, "`pad_type` requires `pad = scalar`")
                .to_compile_error()
                .into()
        }
        (Some((PadMode::Scalar, _)), Some((scalar, _))) => layout::Pad::Scalar(scalar),
        (Some((PadMode::Scalar, _)), None) => layout::Pad::Scalar(DEFAULT_PAD_TYPE),
    };

    let mut item_struct = parse_macro_input!(item as ItemStruct);

    // The markers live on the fields, so the layout has to be generated before
    // they are stripped from the struct that is emitted back to the compiler.
    let input = derive_input_of(&item_struct);
    let generated = layout::emit(
        input,
        layout::Options {
            sort,
            pad,
            ..options
        },
    )
    .unwrap_or_else(|error| error.to_compile_error());

    retain_non_marker_attrs(&mut item_struct);

    quote! {
        #item_struct
        #generated
    }
    .into()
}

/// How padding is represented: `array` keeps `[u8; N]` fields, `scalar` writes
/// one field per chunk.
#[derive(Clone, Copy)]
enum PadMode {
    Array,
    Scalar,
}

/// Padding type used by `pad = scalar` when `pad_type` is not given.
const DEFAULT_PAD_TYPE: layout::ScalarPad = layout::ScalarPad {
    name: "u32",
    size: 4,
    align: 4,
};

fn option_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    let value = meta.value()?;
    if value.peek(syn::LitStr) {
        Ok(value.parse::<syn::LitStr>()?.value())
    } else {
        Ok(value.parse::<syn::Ident>()?.to_string())
    }
}

fn parse_pad_mode(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<PadMode> {
    let name = option_value(meta)?;

    match name.as_str() {
        "array" => Ok(PadMode::Array),
        "scalar" => Ok(PadMode::Scalar),
        other => Err(meta.error(format!(
            "unsupported padding mode `{other}`, expected `array` or `scalar`"
        ))),
    }
}

fn parse_pad_type(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<layout::ScalarPad> {
    let name = option_value(meta)?;

    let scalar = match name.as_str() {
        "u16" => layout::ScalarPad {
            name: "u16",
            size: 2,
            align: 2,
        },
        "u32" => layout::ScalarPad {
            name: "u32",
            size: 4,
            align: 4,
        },
        "f32" => layout::ScalarPad {
            name: "f32",
            size: 4,
            align: 4,
        },
        other => {
            return Err(meta.error(format!(
                "unsupported padding type `{other}`, expected `u16`, `u32` or `f32`"
            )))
        }
    };

    Ok(scalar)
}

fn derive_input_of(item_struct: &ItemStruct) -> DeriveInput {
    DeriveInput {
        attrs: item_struct.attrs.clone(),
        vis: item_struct.vis.clone(),
        ident: item_struct.ident.clone(),
        generics: item_struct.generics.clone(),
        data: syn::Data::Struct(syn::DataStruct {
            struct_token: item_struct.struct_token,
            fields: item_struct.fields.clone(),
            semi_token: item_struct.semi_token,
        }),
    }
}

fn retain_non_marker_attrs(item_struct: &mut ItemStruct) {
    let syn::Fields::Named(fields) = &mut item_struct.fields else {
        return;
    };

    for field in fields.named.iter_mut() {
        field.attrs.retain(|attr| !attr.path().is_ident("crevice"));
    }
}

#[proc_macro_derive(AsStd140, attributes(crevice))]
pub fn derive_as_std140(input: CompilerTokenStream) -> CompilerTokenStream {
    expand_derive(input, STD140)
}

#[proc_macro_derive(AsStd430, attributes(crevice))]
pub fn derive_as_std430(input: CompilerTokenStream) -> CompilerTokenStream {
    expand_derive(input, STD430)
}

/// Replaces `#[derive(AsStd140)]` and accepts layout options.
///
/// ```ignore
/// #[crevice::std140]
/// pub struct Params {
///     pub resolution: Vec2,
///     #[crevice(size = 16)]
///     pub camera: Camera,
/// }
/// ```
///
/// Options:
///
/// * `sort`: lay fields out by descending alignment, which removes padding that
///   the declaration order would otherwise introduce.
/// * `pad = array|scalar`: how padding is represented. `array` (the default)
///   writes `[u8; N]` fields, `scalar` writes one field per chunk of the
///   selected padding type.
/// * `pad_type = u16|u32|f32`: padding type for `pad = scalar`, `u32` by
///   default. Shader compilers such as naga reject byte arrays in buffer
///   blocks, so scalar padding is the mode to use when the generated struct is
///   also the shader type. `u16` padding needs an even padding length and a
///   shader compiler that accepts 16-bit integers in buffer blocks
///   (`SHADER_INT16` in naga); `u32` and `f32` fit every layout without extra
///   capabilities. Padding that cannot be covered exactly, or that starts at an
///   offset which is not a multiple of the type's alignment, is rejected with a
///   compile error.
///
/// `sort` and `pad = scalar` require a known size for every field, so fields
/// whose size cannot be inferred need `#[crevice(size = ..)]`.
#[proc_macro_attribute]
pub fn std140(args: CompilerTokenStream, item: CompilerTokenStream) -> CompilerTokenStream {
    expand_attribute(args, item, STD140)
}

/// Replaces `#[derive(AsStd430)]` and accepts layout options.
///
/// See [`std140`] for the supported options.
#[proc_macro_attribute]
pub fn std430(args: CompilerTokenStream, item: CompilerTokenStream) -> CompilerTokenStream {
    expand_attribute(args, item, STD430)
}

#[proc_macro_derive(GlslStruct)]
pub fn derive_glsl_struct(input: CompilerTokenStream) -> CompilerTokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let expanded = glsl::emit(input);

    CompilerTokenStream::from(expanded)
}
