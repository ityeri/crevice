use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{parse_quote, Data, DeriveInput, Field, Fields, Ident, LitStr, Path, Result, Type};

use crate::field::{self, Marked};

/// Options shared by the derive and attribute entry points.
pub struct Options {
    pub trait_name: &'static str,
    pub mod_name: &'static str,
    pub min_struct_alignment: usize,
    /// Sort fields by descending alignment (then size) before laying them out.
    pub sort: bool,
}

struct FieldInfo<'a> {
    field: &'a Field,
    name: &'a Ident,
    /// Layout written by the user through `#[crevice(size = ..)]`.
    explicit: Option<Marked>,
    /// Layout known at expansion time, either explicit or inferred.
    resolved: Option<Marked>,
}

pub fn emit(input: DeriveInput, options: Options) -> Result<TokenStream> {
    let Options {
        trait_name,
        mod_name,
        min_struct_alignment,
        sort,
    } = options;

    let mod_name = Ident::new(mod_name, Span::call_site());
    let trait_name = Ident::new(trait_name, Span::call_site());

    let mod_path: Path = parse_quote!(::crevice::#mod_name);
    let trait_path: Path = parse_quote!(#mod_path::#trait_name);

    let as_trait_name = format_ident!("As{}", trait_name);
    let as_trait_path: Path = parse_quote!(#mod_path::#as_trait_name);
    let as_trait_method = format_ident!("as_{}", mod_name);
    let from_trait_method = format_ident!("from_{}", mod_name);

    let visibility = input.vis;
    let input_name = input.ident.clone();
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let has_generics = !input.generics.params.is_empty();

    let generated_name = format_ident!("{}{}", trait_name, input_name);

    // Crevice's macros only work on regular structs. We could potentially
    // support transparent tuple structs in the future.
    let named_fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => fields.named.iter().collect::<Vec<_>>(),
            Fields::Unnamed(_) => {
                return Err(syn::Error::new_spanned(
                    &input.ident,
                    "tuple structs are not supported",
                ))
            }
            Fields::Unit => {
                return Err(syn::Error::new_spanned(
                    &input.ident,
                    "unit structs are not supported",
                ))
            }
        },
        Data::Enum(_) | Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "only structs are supported",
            ))
        }
    };

    let mut infos: Vec<FieldInfo<'_>> = named_fields
        .into_iter()
        .map(|field| {
            let explicit = field::parse(field)?;
            let resolved = explicit.or_else(|| field::infer(&field.ty));
            let name = field
                .ident
                .as_ref()
                .expect("named fields always have a name");
            Ok(FieldInfo {
                field,
                name,
                explicit,
                resolved,
            })
        })
        .collect::<Result<_>>()?;

    if sort {
        if let Some(unknown) = infos.iter().find(|info| info.resolved.is_none()) {
            let name = unknown.name;
            let message = format!(
                "`sort` needs the layout size of every field at macro expansion time, \
                 but `{name}` has none; add `#[crevice(size = ..)]` to it"
            );
            return Err(syn::Error::new_spanned(unknown.field, message));
        }

        infos.sort_by_key(|info| {
            let marked = info.resolved.expect("`sort` checked every field above");
            (std::cmp::Reverse(marked.align), std::cmp::Reverse(marked.size))
        });
    }

    let field_count = infos.len();

    let layout_version_of_ty = |ty: &Type| {
        quote! {
            <#ty as #as_trait_path>::Output
        }
    };

    // Size of a field in bytes: a literal when the value is known at expansion
    // time, otherwise an expression the compiler evaluates for us.
    let size_of_field = |info: &FieldInfo<'_>| match info.resolved {
        Some(marked) => {
            let size = marked.size;
            quote!(#size)
        }
        None => {
            let layout_ty = layout_version_of_ty(&info.field.ty);
            quote!(::core::mem::size_of::<#layout_ty>())
        }
    };

    // Alignment of a field in bytes, same strategy as `size_of_field`.
    let alignment_of_field = |info: &FieldInfo<'_>| match info.resolved {
        Some(marked) => {
            let align = marked.align;
            quote!(#align)
        }
        None => {
            let layout_ty = layout_version_of_ty(&info.field.ty);
            quote!(<#layout_ty as #trait_path>::ALIGNMENT)
        }
    };

    let field_alignments = infos.iter().map(&alignment_of_field);
    let struct_alignment = quote! {
        ::crevice::internal::max_arr([
            #min_struct_alignment,
            #(#field_alignments,)*
        ])
    };

    // Generate names for each padding calculation function.
    let pad_fns: Vec<_> = (0..field_count)
        .map(|index| format_ident!("_{}__{}Pad{}", input_name, trait_name, index))
        .collect();

    // Computes the offset immediately AFTER the field with the given index.
    //
    // This function depends on the generated padding calculation functions to
    // do correct alignment. Be careful not to cause recursion!
    let offset_after_field = |target: usize| {
        let mut output = vec![quote!(0usize)];

        for (index, info) in infos.iter().enumerate().take(target + 1) {
            let field_size = size_of_field(info);
            output.push(quote! {
                + #field_size
            });

            // For every field except our target field, also add the generated
            // padding. Padding occurs after each field, so it isn't included in
            // this value.
            if index < target {
                let pad_fn = &pad_fns[index];
                output.push(quote! {
                    + #pad_fn()
                });
            }
        }

        output.into_iter().collect::<TokenStream>()
    };

    let pad_fn_impls: TokenStream = pad_fns
        .iter()
        .enumerate()
        .map(|(index, pad_fn)| {
            let starting_offset = offset_after_field(index);

            let next_field_or_self_alignment = infos
                .get(index + 1)
                .map(&alignment_of_field)
                .unwrap_or(quote!(#struct_alignment));

            quote! {
                /// Tells how many bytes of padding have to be inserted after
                /// the field with index #index.
                #[allow(non_snake_case)]
                const fn #pad_fn() -> usize {
                    // First up, calculate our offset into the struct so far.
                    // We'll use this value to figure out how far out of
                    // alignment we are.
                    let starting_offset = #starting_offset;

                    // We set our target alignment to the larger of the
                    // alignment due to the previous field and the alignment
                    // requirement of the next field.
                    let alignment = #next_field_or_self_alignment;

                    // Using everything we've got, compute our padding amount.
                    ::crevice::internal::align_offset(starting_offset, alignment)
                }
            }
        })
        .collect();

    let generated_struct_fields: TokenStream = infos
        .iter()
        .enumerate()
        .map(|(index, info)| {
            let field_name = info.name;
            let field_vis = &info.field.vis;
            let field_ty = layout_version_of_ty(&info.field.ty);
            let pad_field_name = format_ident!("_pad{}", index);
            let pad_fn = &pad_fns[index];

            quote! {
                #field_vis #field_name: #field_ty,
                #pad_field_name: [u8; #pad_fn()],
            }
        })
        .collect();

    let generated_struct_field_init: TokenStream = infos
        .iter()
        .map(|info| {
            let field_name = info.name;

            quote! {
                #field_name: self.#field_name.#as_trait_method(),
            }
        })
        .collect();

    let input_struct_field_init: TokenStream = infos
        .iter()
        .map(|info| {
            let field_name = info.name;

            quote! {
                #field_name: #as_trait_path::#from_trait_method(input.#field_name),
            }
        })
        .collect();

    let struct_definition = quote! {
        #[derive(Debug, Clone, Copy)]
        #[repr(C)]
        #[doc(hidden)]
        #[allow(warnings)]
        #visibility struct #generated_name #ty_generics #where_clause {
            #generated_struct_fields
        }
    };

    // Re-checks every layout number that did not come from the compiler, and
    // the resulting struct size. Skipped for generic structs: their field sizes
    // depend on the type parameters.
    let layout_assertions = if has_generics {
        quote!()
    } else {
        let mut assertions = Vec::new();

        for info in &infos {
            let Some(marked) = info.resolved else {
                continue;
            };

            let field_ty = &info.field.ty;
            let layout_ty = layout_version_of_ty(field_ty);
            let field_name = info.name;
            let size = marked.size;
            let align = marked.align;

            let hint = if info.explicit.is_some() {
                "does not match the layout"
            } else {
                "was inferred but does not match the layout; add `#[crevice(size = ..)]`"
            };

            let size_message = LitStr::new(
                &format!(
                    "field `{field_name}`: declared size {size} {hint} size of `{}`",
                    quote!(#field_ty)
                ),
                Span::call_site(),
            );
            let align_message = LitStr::new(
                &format!(
                    "field `{field_name}`: declared alignment {align} {hint} alignment of `{}`",
                    quote!(#field_ty)
                ),
                Span::call_site(),
            );

            assertions.push(quote! {
                const _: () = {
                    assert!(
                        ::core::mem::size_of::<#layout_ty>() == #size,
                        #size_message
                    );
                    assert!(
                        <#layout_ty as #trait_path>::ALIGNMENT == #align,
                        #align_message
                    );
                };
            });
        }

        let all_known: Option<Vec<Marked>> = infos.iter().map(|info| info.resolved).collect();

        if let Some(marked) = all_known {
            let sizes: Vec<usize> = marked.iter().map(|marked| marked.size).collect();
            let aligns: Vec<usize> = marked.iter().map(|marked| marked.align).collect();
            let (_, total, _) = numeric_layout(&sizes, &aligns, min_struct_alignment);
            let size_message = LitStr::new(
                &format!(
                    "`{generated_name}` size does not match the computed layout ({total} bytes                      from the field sizes and alignments)"
                ),
                Span::call_site(),
            );

            assertions.push(quote! {
                const _: () = {
                    assert!(
                        ::core::mem::size_of::<#generated_name #ty_generics>() == #total,
                        #size_message
                    );
                };
            });
        }

        quote!(#(#assertions)*)
    };

    let debug_methods = if cfg!(feature = "debug-methods") {
        let debug_fields: TokenStream = infos
            .iter()
            .map(|info| {
                let field_name = info.name;
                let field_ty = &info.field.ty;

                quote! {
                    fields.push(Field {
                        name: stringify!(#field_name),
                        size: ::core::mem::size_of::<#field_ty>(),
                        offset: (&zeroed.#field_name as *const _ as usize)
                            - (&zeroed as *const _ as usize),
                    });
                }
            })
            .collect();

        quote! {
            impl #impl_generics #generated_name #ty_generics #where_clause {
                fn debug_metrics() -> String {
                    let size = ::core::mem::size_of::<Self>();
                    let align = <Self as #trait_path>::ALIGNMENT;

                    let zeroed: Self = ::crevice::internal::bytemuck::Zeroable::zeroed();

                    #[derive(Debug)]
                    struct Field {
                        name: &'static str,
                        offset: usize,
                        size: usize,
                    }
                    let mut fields = Vec::new();

                    #debug_fields

                    format!("Size {}, Align {}, fields: {:#?}", size, align, fields)
                }

                fn debug_definitions() -> &'static str {
                    stringify!(
                        #struct_definition
                        #pad_fn_impls
                    )
                }
            }
        }
    } else {
        quote!()
    };

    Ok(quote! {
        #pad_fn_impls
        #struct_definition

        #layout_assertions

        impl #impl_generics #generated_name #ty_generics #where_clause {
            #[allow(warnings)]
            pub fn as_bytes(&self) -> &[u8] {
                <#generated_name #ty_generics as #trait_path>::as_bytes(self)
            }
        }

        unsafe impl #impl_generics ::crevice::internal::bytemuck::Zeroable for #generated_name #ty_generics #where_clause {}
        unsafe impl #impl_generics ::crevice::internal::bytemuck::Pod for #generated_name #ty_generics #where_clause {}

        unsafe impl #impl_generics #trait_path for #generated_name #ty_generics #where_clause {
            const ALIGNMENT: usize = #struct_alignment;
        }

        impl #impl_generics #as_trait_path for #input_name #ty_generics #where_clause {
            type Output = #generated_name;

            fn #as_trait_method(&self) -> Self::Output {
                Self::Output {
                    #generated_struct_field_init

                    ..::crevice::internal::bytemuck::Zeroable::zeroed()
                }
            }

            fn #from_trait_method(input: Self::Output) -> Self {
                Self {
                    #input_struct_field_init
                }
            }
        }

        #debug_methods
    })
}

/// Offsets and total size the generated padding functions produce.
///
/// Only usable when every field size is known at expansion time; the caller
/// emits an assertion comparing this against the real struct size.
fn numeric_layout(
    sizes: &[usize],
    aligns: &[usize],
    min_struct_alignment: usize,
) -> (Vec<usize>, usize, usize) {
    let struct_alignment = aligns
        .iter()
        .copied()
        .fold(min_struct_alignment, usize::max);

    let mut offsets = Vec::with_capacity(sizes.len());
    let mut offset = 0usize;

    for index in 0..sizes.len() {
        offsets.push(offset);
        offset += sizes[index];

        let next_alignment = aligns.get(index + 1).copied().unwrap_or(struct_alignment);
        offset = align_up(offset, next_alignment);
    }

    (offsets, offset, struct_alignment)
}

fn align_up(value: usize, alignment: usize) -> usize {
    if alignment <= 1 {
        value
    } else {
        (value + alignment - 1) / alignment * alignment
    }
}
