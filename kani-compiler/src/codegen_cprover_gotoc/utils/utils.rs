// Copyright Kani Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
use super::super::codegen::TypeExt;
use crate::codegen_cprover_gotoc::codegen::typ::{is_pointer, pointee_type};
use crate::codegen_cprover_gotoc::codegen::PropertyClass;
use crate::codegen_cprover_gotoc::GotocCtx;
use cbmc::goto_program::{Expr, ExprValue, Location, Stmt, SymbolTable, Type};
use cbmc::{btree_string_map, InternedString};
use rustc_errors::FatalError;
use rustc_middle::ty::layout::LayoutOf;
use rustc_middle::ty::{Instance, Ty};
use tracing::debug;

// Should move into rvalue
//make this a member function
pub fn slice_fat_ptr(typ: Type, data: Expr, len: Expr, symbol_table: &SymbolTable) -> Expr {
    Expr::struct_expr(typ, btree_string_map![("data", data), ("len", len)], symbol_table)
}

pub fn dynamic_fat_ptr(typ: Type, data: Expr, vtable: Expr, symbol_table: &SymbolTable) -> Expr {
    Expr::struct_expr(typ, btree_string_map![("data", data), ("vtable", vtable)], symbol_table)
}

/// Tries to extract a string message from an `Expr`.
/// If the expression represents a pointer to a string constant, this will return the string
/// constant. Otherwise, return `None`.
pub fn extract_const_message(arg: &Expr) -> Option<String> {
    match arg.value() {
        ExprValue::Struct { values } => match &values[0].value() {
            ExprValue::AddressOf(address) => match address.value() {
                ExprValue::Index { array, .. } => match array.value() {
                    ExprValue::StringConstant { s } => Some(s.to_string()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

impl<'tcx> GotocCtx<'tcx> {
    /// Kani does not currently support all MIR constructs.
    /// When we hit a construct we don't handle, we have two choices:
    /// We can use the `unimplemented!()` macro, which causes a compile time failure.
    /// Or, we can use this function, which inserts an `assert(false, "FOO is not currently supported by Kani")` into the generated code.
    /// This means that if the unimplemented feature is dynamically used by the code being verified, we will see an assertion failure.
    /// If it is not used, we the assertion will pass.
    /// This allows us to continue to make progress parsing rust code, while remaining sound (thanks to the `assert(false)`)
    pub fn codegen_unimplemented(
        &mut self,
        operation_name: &str,
        t: Type,
        loc: Location,
        url: &str,
    ) -> Expr {
        // Save this occurrence so we can emit a warning in the compilation report.
        debug!("codegen_unimplemented: {} at {}", operation_name, loc.short_string());
        let key: InternedString = operation_name.into();
        if !self.unsupported_constructs.contains_key(&key) {
            self.unsupported_constructs.insert(key, Vec::new());
        }
        self.unsupported_constructs.get_mut(&key).unwrap().push(loc);

        let body = vec![
            // Assert false to alert the user that there is a path that uses an unimplemented feature.
            self.codegen_assert_false(
                PropertyClass::UnsupportedConstruct,
                &GotocCtx::unsupported_msg(operation_name, Some(url)),
                loc,
            ),
            // Assume false to block any further exploration of this path.
            Stmt::assume(Expr::bool_false(), loc),
            t.nondet().as_stmt(loc).with_location(loc), //TODO assume rust validity contraints
        ];

        Expr::statement_expression(body, t).with_location(loc)
    }

    /// Generates an expression `(ptr as usize) % align_of(T) == 0`
    /// to determine if a pointer `ptr` with pointee type `T` is aligned.
    pub fn is_ptr_aligned(&mut self, typ: Ty<'tcx>, ptr: Expr) -> Expr {
        // Ensure `typ` is a pointer, then extract the pointee type
        assert!(is_pointer(typ));
        let pointee_type = pointee_type(typ).unwrap();
        // Obtain the alignment for the pointee type `T`
        let layout = self.layout_of(pointee_type);
        let align = Expr::int_constant(layout.align.abi.bytes(), Type::size_t());
        // Cast the pointer to `usize` and return the alignment expression
        let cast_ptr = ptr.cast_to(Type::size_t());
        let zero = Type::size_t().zero();
        cast_ptr.rem(align).eq(zero)
    }

    pub fn unsupported_msg(item: &str, url: Option<&str>) -> String {
        let mut s = format!("{} is not currently supported by Kani", item);
        if let Some(url) = url {
            s.push_str(". Please post your example at ");
            s.push_str(url);
        }
        s
    }

    pub fn emit_error_and_exit(&self, error_msg: &str) -> ! {
        self.tcx.sess.err(error_msg);
        FatalError.raise()
    }
}

/// Members traverse path to get to the raw pointer of a box (b.0.pointer.pointer).
const RAW_PTR_FROM_BOX: [&str; 3] = ["0", "pointer", "pointer"];

impl<'tcx> GotocCtx<'tcx> {
    /// Given an "instance" find the crate it came from
    pub fn get_crate(&self, instance: Instance<'tcx>) -> String {
        self.tcx.crate_name(instance.def_id().krate).to_string()
    }
}

impl<'tcx> GotocCtx<'tcx> {
    /// Dereference a boxed type `std::boxed::Box<T>` to get a `*T`.
    ///
    /// WARNING: This is based on a manual inspection of how boxed types are currently
    /// a) implemented by the rust standard library
    /// b) codegenned by Kani.
    /// If either of those change, this will almost certainly stop working.
    pub fn deref_box(&self, box_expr: Expr) -> Expr {
        // Internally, a Boxed type is stored as a chain of structs.
        //
        // This code has to match the exact structure from the std library version that is
        // supported to access the raw pointer. If either rustc or Kani changes how boxed types are
        // represented, this will need to be updated.
        self.assert_is_rust_box_like(box_expr.typ());
        RAW_PTR_FROM_BOX.iter().fold(box_expr, |expr, name| expr.member(name, &self.symbol_table))
    }

    /// Box<T> initializer
    ///
    /// Traverse over the Box representation and only initialize the raw_ptr field. All other
    /// members are left uninitialized.
    /// `boxed_type` is the type of the resulting expression
    pub fn box_value(&self, boxed_value: Expr, boxed_type: Type) -> Expr {
        self.assert_is_rust_box_like(&boxed_type);
        tracing::debug!(?boxed_type, ?boxed_value, "box_value");
        let mut inner_type = boxed_type;
        let type_members = RAW_PTR_FROM_BOX
            .iter()
            .map(|name| {
                let outer_type = inner_type.clone();
                inner_type = outer_type.lookup_field_type(name, &self.symbol_table).unwrap();
                (*name, outer_type)
            })
            .collect::<Vec<_>>();

        type_members.iter().rev().fold(boxed_value, |value, (name, typ)| {
            Expr::struct_expr_with_nondet_fields(
                typ.clone(),
                btree_string_map![(*name, value),],
                &self.symbol_table,
            )
        })
    }

    /// Best effort check if the struct represents a rust "std::alloc::Global".
    fn assert_is_rust_global_alloc_like(&self, t: &Type) {
        // TODO: A std::alloc::Global appears to be an empty struct, in the cases we've seen.
        // Is there something smarter we can do here?
        assert!(t.is_struct_like());
        let components = t.lookup_components(&self.symbol_table).unwrap();
        assert_eq!(components.len(), 0);
    }

    /// Best effort check if the struct represents a rust "std::marker::PhantomData".
    fn assert_is_rust_phantom_data_like(&self, t: &Type) {
        // TODO: A std::marker::PhantomData appears to be an empty struct, in the cases we've seen.
        // Is there something smarter we can do here?
        assert!(t.is_struct_like());
        let components = t.lookup_components(&self.symbol_table).unwrap();
        assert_eq!(components.len(), 0);
    }

    /// Best effort check if the struct represents a Rust "Box". May return false positives.
    fn assert_is_rust_box_like(&self, t: &Type) {
        // struct std::boxed::Box<[u8; 8]>::15334369982748499855
        // {
        //   // 1
        //   struct std::alloc::Global::13633191317886109837 1;
        //   // 0
        //   struct std::ptr::Unique<[u8; 8]>::14713681870393313245 0;
        // };
        assert!(t.is_struct_like());
        let components = t.lookup_components(&self.symbol_table).unwrap();
        assert_eq!(components.len(), 2);
        for c in components {
            match c.name().to_string().as_str() {
                "0" => self.assert_is_rust_unique_pointer_like(&c.typ()),
                "1" => self.assert_is_rust_global_alloc_like(&c.typ()),
                _ => panic!("Unexpected component {} in {:?}", c.name(), t),
            }
        }
    }

    /// Checks if the struct represents a Rust "std::ptr::Unique"
    fn assert_is_rust_unique_pointer_like(&self, t: &Type) {
        // struct std::ptr::Unique<[u8; 8]>::14713681870393313245
        // {
        //   // _marker
        //   struct std::marker::PhantomData<[u8; 8]>::18073278521438838603 _marker;
        //   // pointer
        //   NonNull<T> pointer;
        // };
        assert!(t.is_struct_like());
        let components = t.lookup_components(&self.symbol_table).unwrap();
        assert_eq!(components.len(), 2);
        for c in components {
            match c.name().to_string().as_str() {
                "_marker" => self.assert_is_rust_phantom_data_like(&c.typ()),
                "pointer" => self.assert_is_non_null_like(&c.typ()),
                _ => panic!("Unexpected component {} in {:?}", c.name(), t),
            }
        }
    }

    /// Best effort check if the struct represents a std::ptr::NonNull<T>.
    ///
    /// This assumes the following structure. Any changes to this will break this code.
    /// ```
    /// pub struct NonNull<T: ?Sized> {
    ///    pointer: *const T,
    /// }
    /// ```
    fn assert_is_non_null_like(&self, t: &Type) {
        assert!(t.is_struct_like());
        let components = t.lookup_components(&self.symbol_table).unwrap();
        assert_eq!(components.len(), 1);
        let component = components.first().unwrap();
        assert_eq!(component.name().to_string().as_str(), "pointer");
        assert!(component.typ().is_pointer() || component.typ().is_rust_fat_ptr(&self.symbol_table))
    }
}
