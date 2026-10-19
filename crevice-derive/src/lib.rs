mod field;
mod glsl;
mod layout;

use proc_macro::TokenStream as CompilerTokenStream;

use quote::quote;
use syn::{parse_macro_input, DeriveInput, ItemStruct};

const STD140: layout::Options = layout::Options {
    trait_name: "Std140",
    mod_name: "std140",
    min_struct_alignment: 16,
    sort: false,
};

const STD430: layout::Options = layout::Options {
    trait_name: "Std430",
    mod_name: "std430",
    min_struct_alignment: 0,
    sort: false,
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
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("sort") {
            if sort {
                return Err(meta.error("duplicate `sort` option"));
            }
            sort = true;
            Ok(())
        } else {
            Err(meta.error("unsupported option, expected `sort`"))
        }
    });
    parse_macro_input!(args with parser);

    let mut item_struct = parse_macro_input!(item as ItemStruct);

    // The markers live on the fields, so the layout has to be generated before
    // they are stripped from the struct that is emitted back to the compiler.
    let input = derive_input_of(&item_struct);
    let generated = layout::emit(
        input,
        layout::Options {
            sort,
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
///   the declaration order would otherwise introduce. Requires a known size for
///   every field, so fields whose size cannot be inferred need
///   `#[crevice(size = ..)]`.
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
