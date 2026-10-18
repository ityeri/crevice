use proc_macro::TokenStream;
use proc_macro2::{Ident, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{parse_macro_input, FnArg, ItemFn, Pat, Path, PathArguments, Type, TypePath};

/// Replaces `Std140Foo` / `Std430Foo` parameters with their original type by
/// inserting `let name: Foo = Foo::from_std140(*name);` at the top of the body.
pub fn emit(item: TokenStream) -> TokenStream {
    let mut func = parse_macro_input!(item as ItemFn);
    let mut conversions = Vec::new();
    let mut error = None;

    for arg in func.sig.inputs.iter_mut() {
        let pat_type = match arg {
            FnArg::Typed(pat_type) => pat_type,
            FnArg::Receiver(_) => continue,
        };

        let mut explicit = None;
        pat_type.attrs.retain(|attr| {
            if attr.path().is_ident("original") {
                match attr.parse_args::<Type>() {
                    Ok(ty) => explicit = Some(ty),
                    Err(err) => error = Some(err),
                }
                false
            } else {
                true
            }
        });

        let (value_ty, by_ref, by_mut) = match &*pat_type.ty {
            Type::Reference(reference) => (&*reference.elem, true, reference.mutability.is_some()),
            other => (other, false, false),
        };

        let conversion = match explicit {
            Some(original) => Some((
                original,
                quote!(::crevice::std140::AsStd140),
                format_ident!("from_std140"),
            )),
            None => layout_original(value_ty),
        };

        let Some((original, as_trait, from_fn)) = conversion else {
            continue;
        };

        if by_mut {
            error = Some(syn::Error::new_spanned(
                &pat_type.ty,
                "#[crevice::from_layout] cannot convert `&mut` layout parameters",
            ));
            continue;
        }

        let deref = if by_ref { quote!(*) } else { quote!() };

        if let Pat::Ident(pat) = &*pat_type.pat {
            let name = &pat.ident;
            conversions.push(quote! {
                let #name: #original = <#original as #as_trait>::#from_fn(#deref #name);
            });
        }
    }

    if let Some(err) = error {
        return err.to_compile_error().into();
    }

    let attrs = &func.attrs;
    let vis = &func.vis;
    let sig = &func.sig;
    let stmts = &func.block.stmts;

    quote! {
        #(#attrs)*
        #vis #sig {
            #(#conversions)*
            #(#stmts)*
        }
    }
    .into()
}

/// `Std140Foo`/`Std430Foo` -> `Foo` plus the matching `AsStd*` trait.
fn layout_original(ty: &Type) -> Option<(Type, TokenStream2, Ident)> {
    let Type::Path(type_path) = ty else {
        return None;
    };

    if type_path.qself.is_some() {
        return None;
    }

    let last = type_path.path.segments.last()?;

    if !matches!(last.arguments, PathArguments::None) {
        return None;
    }

    let name = last.ident.to_string();

    let (rest, as_trait, from_fn) = if let Some(rest) = name.strip_prefix("Std140") {
        (rest, quote!(::crevice::std140::AsStd140), format_ident!("from_std140"))
    } else if let Some(rest) = name.strip_prefix("Std430") {
        (rest, quote!(::crevice::std430::AsStd430), format_ident!("from_std430"))
    } else {
        return None;
    };

    if rest.is_empty() {
        return None;
    }

    let ident = format_ident!("{}", rest);
    let last_index = type_path.path.segments.len() - 1;
    let mut segments = Punctuated::new();

    for (index, segment) in type_path.path.segments.iter().enumerate() {
        let mut segment = segment.clone();

        if index == last_index {
            segment.ident = ident.clone();
        }

        segments.push(segment);
    }

    let path = Path {
        leading_colon: type_path.path.leading_colon,
        segments,
    };

    Some((Type::Path(TypePath { qself: None, path }), as_trait, from_fn))
}
