use syn::{Field, LitInt, Result, Type};

/// Layout numbers for a single field, in bytes.
#[derive(Clone, Copy, Debug)]
pub struct Marked {
    pub size: usize,
    pub align: usize,
}

/// Alignment used when `#[crevice(size = ..)]` omits `align`.
///
/// std140 rounds structs, arrays and matrices up to 16, and no std140 type has
/// a size of 8 with an alignment of 16, so `min(16, next_power_of_two(size))`
/// is exact for std140. Layouts that disagree trip the generated assertion.
pub fn derived_align(size: usize) -> usize {
    std::cmp::min(16, size.next_power_of_two())
}

/// Reads `#[crevice(size = N, align = M)]` from a field.
pub fn parse(field: &Field) -> Result<Option<Marked>> {
    let mut size: Option<usize> = None;
    let mut align: Option<usize> = None;

    for attr in field.attrs.iter().filter(|attr| attr.path().is_ident("crevice")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("size") {
                let lit: LitInt = meta.value()?.parse()?;
                let value = lit.base10_parse::<usize>()?;
                if value == 0 {
                    return Err(meta.error("`size` must be greater than zero"));
                }
                size = Some(value);
                Ok(())
            } else if meta.path.is_ident("align") {
                let lit: LitInt = meta.value()?.parse()?;
                let value = lit.base10_parse::<usize>()?;
                if !value.is_power_of_two() {
                    return Err(meta.error("`align` must be a power of two"));
                }
                align = Some(value);
                Ok(())
            } else {
                Err(meta.error("unsupported option, expected `size` or `align`"))
            }
        })?;
    }

    match (size, align) {
        (None, None) => Ok(None),
        (Some(size), None) => Ok(Some(Marked {
            size,
            align: derived_align(size),
        })),
        (Some(size), Some(align)) => Ok(Some(Marked { size, align })),
        (None, Some(_)) => Err(syn::Error::new_spanned(
            field,
            "`#[crevice(align = ..)]` requires `size`",
        )),
    }
}

/// Layout of the types crevice can resolve without a manual marker.
///
/// Kept deliberately small: every inferred value is re-checked by a generated
/// assertion, so a wrong guess is a compile error instead of a silent mismatch.
pub fn infer(ty: &Type) -> Option<Marked> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    if type_path.qself.is_some() {
        return None;
    }
    let segment = type_path.path.segments.last()?;
    if !matches!(segment.arguments, syn::PathArguments::None) {
        return None;
    }
    let (size, align) = match segment.ident.to_string().as_str() {
        "f32" | "u32" | "i32" => (4, 4),
        "f64" => (8, 8),
        _ => return None,
    };
    Some(Marked { size, align })
}
