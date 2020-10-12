use ykpack;

pub trait SirMethods {
    fn define_sir_type(&self, ty: ykpack::Ty) -> ykpack::TypeId;
    fn define_function_sir(&self, sir: ykpack::Body);
}
