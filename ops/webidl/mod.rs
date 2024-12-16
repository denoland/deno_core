use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::quote;
use quote::ToTokens;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::parse2;
use syn::spanned::Spanned;
use syn::Attribute;
use syn::Data;
use syn::DeriveInput;
use syn::Error;
use syn::Expr;
use syn::Field;
use syn::Fields;
use syn::Type;

pub fn webidl(item: TokenStream) -> Result<TokenStream, Error> {
  let input = parse2::<DeriveInput>(item)?;
  let span = input.span();
  let ident = input.ident;
  let ident_string = ident.to_string();
  let converter = input
    .attrs
    .into_iter()
    .find_map(|attr| ConverterType::from_attribute(attr).transpose())
    .ok_or_else(|| Error::new(span, "missing #[webidl] attribute"))??;

  let out = match input.data {
    Data::Struct(data) => match converter {
      ConverterType::Dictionary => {
        let fields = match data.fields {
          Fields::Named(fields) => fields,
          Fields::Unnamed(_) => {
            return Err(Error::new(
              span,
              "Unnamed fields are currently not supported",
            ))
          }
          Fields::Unit => {
            return Err(Error::new(
              span,
              "Unit fields are currently not supported",
            ))
          }
        };

        let mut fields = fields
          .named
          .into_iter()
          .map(TryInto::try_into)
          .collect::<Result<Vec<DictionaryField>, Error>>()?;
        fields.sort_by(|a, b| a.name.cmp(&b.name));

        let names = fields
          .iter()
          .map(|field| field.name.clone())
          .collect::<Vec<_>>();

        let fields = fields.into_iter().map(|field| {
          let string_name = field.name.to_string();
          let name = field.name;

          let required_or_default = if field.required && field.default_value.is_none() {
            quote! {
              return Err(::deno_core::webidl::WebIdlError::new(
                __prefix,
                __context,
                ::deno_core::webidl::WebIdlErrorKind::DictionaryCannotConvertKey {
                  converter: #ident_string,
                  key: #string_name,
                },
              ));
            }
          } else if let Some(default) = field.default_value {
            default.to_token_stream()
          } else {
            quote! { None }
          };
          
          let val = if field.required {
            quote!(val)
          } else {
            quote!(Some(val))
          };

          quote! {
            let #name = {
              if let Some(__value) = __obj.as_ref().and_then(|__obj| {
                let __key = v8::String::new(__scope, #string_name).unwrap();
                __obj.get(__scope, __key.into())
              }) {
                let val = ::deno_core::webidl::WebIdlConverter::convert(
                  __scope,
                  __value,
                  __prefix.clone(),
                  format!("'{}' of '{}' ({__context})", #string_name, #ident_string).into(),
                )?;
                #val
              } else {
                #required_or_default
              }
            };
          }
        }).collect::<Vec<_>>();

        quote! {
          impl<'a> ::deno_core::webidl::WebIdlConverter<'a> for #ident {
            fn convert(
              __scope: &mut v8::HandleScope<'a>,
              __value: v8::Local<'a, v8::Value>,
              __prefix: std::borrow::Cow<'static, str>,
              __context: std::borrow::Cow<'static, str>
            ) -> Result<Self, ::deno_core::webidl::WebIdlError> {
              let __obj = if __value.is_undefined() || __value.is_null() {
                None
              } else {
                Some(__value.to_object(__scope).ok_or_else(|| {
                  ::deno_core::webidl::WebIdlError::new(
                    __prefix.clone(),
                    __context.clone(),
                    ::deno_core::webidl::WebIdlErrorKind::ConvertToConverterType("dictionary")
                  )
                })?)
              };

              #(#fields)*

              Ok(Self { #(#names),* })
            }
          }
        }
      }
    },
    Data::Enum(_) => {
      return Err(Error::new(span, "Enums are currently not supported"));
    }
    Data::Union(_) => {
      return Err(Error::new(span, "Unions are currently not supported"))
    }
  };

  Ok(out)
}

mod kw {
  syn::custom_keyword!(dictionary);
}

enum ConverterType {
  Dictionary,
}

impl ConverterType {
  fn from_attribute(attr: Attribute) -> Result<Option<Self>, Error> {
    if attr.path().is_ident("webidl") {
      let list = attr.meta.require_list()?;
      let value = list.parse_args::<Self>()?;
      Ok(Some(value))
    } else {
      Ok(None)
    }
  }
}

impl Parse for ConverterType {
  fn parse(input: ParseStream) -> syn::Result<Self> {
    let lookahead = input.lookahead1();

    if lookahead.peek(kw::dictionary) {
      input.parse::<kw::dictionary>()?;
      Ok(Self::Dictionary)
    } else {
      Err(lookahead.error())
    }
  }
}

struct DictionaryField {
  name: Ident,
  default_value: Option<Expr>,
  required: bool,
}

impl TryFrom<Field> for DictionaryField {
  type Error = Error;
  fn try_from(value: Field) -> Result<Self, Self::Error> {
    let is_optional = if let Type::Path(path) = value.ty {
      if let Some(last) = path.path.segments.last() {
        last.ident.to_string() == "Option"
      } else {
        false
      }
    } else {
      false
    };

    let default_value = value
      .attrs
      .into_iter()
      .find_map(|attr| {
        if attr.path().is_ident("webidl") {
          let list = match attr.meta.require_list() {
            Ok(list) => list,
            Err(e) => return Some(Err(e)),
          };
          let name_value = match list.parse_args::<syn::MetaNameValue>() {
            Ok(name_value) => name_value,
            Err(e) => return Some(Err(e)),
          };

          if name_value.path.is_ident("default") {
            Some(Ok(name_value.value))
          } else {
            None
          }
        } else {
          None
        }
      })
      .transpose()?;

    Ok(Self {
      name: value.ident.unwrap(),
      default_value,
      required: !is_optional,
    })
  }
}
