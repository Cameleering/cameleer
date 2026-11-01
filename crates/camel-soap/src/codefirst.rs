//! Code-first service descriptors: the Rust stand-in for a JAX-WS `serviceClass`. A
//! [`ServiceDescriptor`] bound in the registry (`serviceClass=#calculator`) describes the
//! operations with typed parameters; the cxf component generates a document/literal
//! wrapped WSDL (with its XML Schema) from it, serves it at `?wsdl` and uses it exactly like
//! a `wsdlURL`.
//!
//! ```
//! use camel_soap::codefirst::{OperationDescriptor, ServiceDescriptor};
//! let svc = ServiceDescriptor::new("Calculator", "urn:calc")
//!     .operation(OperationDescriptor::new("add").param("a", "int").param("b", "int").returns("int"))
//!     .operation(OperationDescriptor::new("log").param("line", "string").one_way());
//! let wsdl = svc.to_wsdl("http://localhost:9000/calc");
//! assert!(wsdl.contains("name=\"addResponse\""));
//! ```
//!
//! Types are XML Schema built-in local names (`string`, `int`, `long`, `double`, `decimal`,
//! `boolean`, `dateTime`, `date`, `base64Binary`...), a `[]` suffix for repeated values,
//! a `?` suffix for optional (nillable) values, or the name of a complex type declared with
//! [`ServiceDescriptor::complex_type`]. Header parameters ([`OperationDescriptor::header`])
//! become `soap:header` parts.

use camel_xml::serialize::escape_attr;

use crate::envelope::SoapVersion;

/// One operation of a code-first service.
#[derive(Clone, Debug, Default)]
pub struct OperationDescriptor {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub returns: Option<String>,
    pub one_way: bool,
    pub headers: Vec<(String, String)>,
    pub soap_action: Option<String>,
    pub faults: Vec<String>,
}

impl OperationDescriptor {
    pub fn new(name: &str) -> OperationDescriptor {
        OperationDescriptor { name: name.to_string(), ..Default::default() }
    }
    pub fn param(mut self, name: &str, ty: &str) -> Self {
        self.params.push((name.to_string(), ty.to_string()));
        self
    }
    pub fn returns(mut self, ty: &str) -> Self {
        self.returns = Some(ty.to_string());
        self
    }
    pub fn one_way(mut self) -> Self {
        self.one_way = true;
        self
    }
    /// A parameter carried in a SOAP header.
    pub fn header(mut self, name: &str, ty: &str) -> Self {
        self.headers.push((name.to_string(), ty.to_string()));
        self
    }
    pub fn soap_action(mut self, action: &str) -> Self {
        self.soap_action = Some(action.to_string());
        self
    }
    /// A declared fault (element `{ns}<name>` with a `message` string).
    pub fn fault(mut self, name: &str) -> Self {
        self.faults.push(name.to_string());
        self
    }
}

/// A code-first service description.
#[derive(Clone, Debug)]
pub struct ServiceDescriptor {
    pub name: String,
    pub namespace: String,
    pub operations: Vec<OperationDescriptor>,
    pub complex_types: Vec<(String, Vec<(String, String)>)>,
    pub soap_version: SoapVersion,
}

impl ServiceDescriptor {
    pub fn new(name: &str, namespace: &str) -> ServiceDescriptor {
        ServiceDescriptor { name: name.to_string(), namespace: namespace.to_string(), operations: Vec::new(), complex_types: Vec::new(), soap_version: SoapVersion::V11 }
    }
    pub fn operation(mut self, op: OperationDescriptor) -> Self {
        self.operations.push(op);
        self
    }
    /// A complex type (a sequence of named fields).
    pub fn complex_type(mut self, name: &str, fields: &[(&str, &str)]) -> Self {
        self.complex_types.push((name.to_string(), fields.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()));
        self
    }
    pub fn soap12(mut self) -> Self {
        self.soap_version = SoapVersion::V12;
        self
    }

    fn element_decl(&self, name: &str, ty: &str) -> String {
        let (base, many) = match ty.strip_suffix("[]") {
            Some(b) => (b, true),
            None => (ty, false),
        };
        let (base, optional) = match base.strip_suffix('?') {
            Some(b) => (b, true),
            None => (base, false),
        };
        let qty = if self.complex_types.iter().any(|(n, _)| n == base) { format!("tns:{}", base) } else { format!("xs:{}", base) };
        let mut s = format!("<xs:element name=\"{}\" type=\"{}\"", escape_attr(name), qty);
        if many {
            s.push_str(" minOccurs=\"0\" maxOccurs=\"unbounded\"");
        } else if optional {
            s.push_str(" minOccurs=\"0\" nillable=\"true\"");
        }
        s.push_str("/>");
        s
    }

    fn wrapper(&self, name: &str, fields: &[(String, String)]) -> String {
        let inner: String = fields.iter().map(|(n, t)| self.element_decl(n, t)).collect();
        format!("<xs:element name=\"{}\"><xs:complexType><xs:sequence>{}</xs:sequence></xs:complexType></xs:element>", escape_attr(name), inner)
    }

    /// Generate the WSDL 1.1 document (document/literal wrapped) for an address.
    pub fn to_wsdl(&self, address: &str) -> String {
        let (soap_ns, soap_prefix) = match self.soap_version {
            SoapVersion::V11 => ("http://schemas.xmlsoap.org/wsdl/soap/", "soap"),
            SoapVersion::V12 => ("http://schemas.xmlsoap.org/wsdl/soap12/", "soap12"),
        };
        let mut schema = String::new();
        for (n, fields) in &self.complex_types {
            let inner: String = fields.iter().map(|(f, t)| self.element_decl(f, t)).collect();
            schema.push_str(&format!("<xs:complexType name=\"{}\"><xs:sequence>{}</xs:sequence></xs:complexType>", escape_attr(n), inner));
        }
        let mut messages = String::new();
        let mut port_ops = String::new();
        let mut binding_ops = String::new();
        let mut header_elems: Vec<String> = Vec::new();
        for op in &self.operations {
            schema.push_str(&self.wrapper(&op.name, &op.params));
            messages.push_str(&format!("<wsdl:message name=\"{n}\"><wsdl:part name=\"parameters\" element=\"tns:{n}\"/>", n = op.name));
            for (h, _) in &op.headers {
                messages.push_str(&format!("<wsdl:part name=\"{h}\" element=\"tns:{h}\"/>", h = h));
            }
            messages.push_str("</wsdl:message>");
            for (h, t) in &op.headers {
                if !header_elems.contains(h) {
                    schema.push_str(&self.element_decl(h, t));
                    header_elems.push(h.clone());
                }
            }
            if !op.one_way {
                let ret: Vec<(String, String)> = op.returns.iter().map(|t| ("return".to_string(), t.clone())).collect();
                schema.push_str(&self.wrapper(&format!("{}Response", op.name), &ret));
                messages.push_str(&format!("<wsdl:message name=\"{n}Response\"><wsdl:part name=\"parameters\" element=\"tns:{n}Response\"/></wsdl:message>", n = op.name));
            }
            for f in &op.faults {
                if !header_elems.contains(f) {
                    schema.push_str(&format!("<xs:element name=\"{}\"><xs:complexType><xs:sequence><xs:element name=\"message\" type=\"xs:string\" minOccurs=\"0\"/></xs:sequence></xs:complexType></xs:element>", escape_attr(f)));
                    messages.push_str(&format!("<wsdl:message name=\"{f}\"><wsdl:part name=\"fault\" element=\"tns:{f}\"/></wsdl:message>", f = f));
                    header_elems.push(f.clone());
                }
            }
            port_ops.push_str(&format!("<wsdl:operation name=\"{n}\"><wsdl:input message=\"tns:{n}\"/>", n = op.name));
            if !op.one_way {
                port_ops.push_str(&format!("<wsdl:output message=\"tns:{n}Response\"/>", n = op.name));
            }
            for f in &op.faults {
                port_ops.push_str(&format!("<wsdl:fault name=\"{f}\" message=\"tns:{f}\"/>", f = f));
            }
            port_ops.push_str("</wsdl:operation>");
            let headers: String = op.headers.iter().map(|(h, _)| format!("<{p}:header message=\"tns:{n}\" part=\"{h}\" use=\"literal\"/>", p = soap_prefix, n = op.name, h = h)).collect();
            binding_ops.push_str(&format!(
                "<wsdl:operation name=\"{n}\"><{p}:operation soapAction=\"{a}\"/><wsdl:input><{p}:body use=\"literal\" parts=\"parameters\"/>{h}</wsdl:input>",
                n = op.name,
                p = soap_prefix,
                a = escape_attr(op.soap_action.as_deref().unwrap_or("")),
                h = headers
            ));
            if !op.one_way {
                binding_ops.push_str(&format!("<wsdl:output><{p}:body use=\"literal\"/></wsdl:output>", p = soap_prefix));
            }
            for f in &op.faults {
                binding_ops.push_str(&format!("<wsdl:fault name=\"{f}\"><{p}:fault name=\"{f}\" use=\"literal\"/></wsdl:fault>", f = f, p = soap_prefix));
            }
            binding_ops.push_str("</wsdl:operation>");
        }
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<wsdl:definitions name=\"{svc}\" targetNamespace=\"{ns}\" xmlns:wsdl=\"http://schemas.xmlsoap.org/wsdl/\" xmlns:{sp}=\"{sns}\" xmlns:tns=\"{ns}\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\
<wsdl:types><xs:schema targetNamespace=\"{ns}\" elementFormDefault=\"qualified\">{schema}</xs:schema></wsdl:types>{messages}\
<wsdl:portType name=\"{svc}PortType\">{port_ops}</wsdl:portType>\
<wsdl:binding name=\"{svc}SoapBinding\" type=\"tns:{svc}PortType\"><{sp}:binding style=\"document\" transport=\"http://schemas.xmlsoap.org/soap/http\"/>{binding_ops}</wsdl:binding>\
<wsdl:service name=\"{svc}\"><wsdl:port name=\"{svc}Port\" binding=\"tns:{svc}SoapBinding\"><{sp}:address location=\"{addr}\"/></wsdl:port></wsdl:service></wsdl:definitions>",
            svc = escape_attr(&self.name),
            ns = escape_attr(&self.namespace),
            sp = soap_prefix,
            sns = soap_ns,
            schema = schema,
            messages = messages,
            port_ops = port_ops,
            binding_ops = binding_ops,
            addr = escape_attr(address),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wsdl::Wsdl;

    #[test]
    fn generated_wsdl_parses() {
        let svc = ServiceDescriptor::new("Shop", "urn:shop")
            .complex_type("Item", &[("sku", "string"), ("qty", "int")])
            .operation(OperationDescriptor::new("order").param("items", "Item[]").param("note", "string?").header("auth", "string").returns("long").fault("OutOfStock"))
            .operation(OperationDescriptor::new("ping").one_way());
        let text = svc.to_wsdl("http://localhost/shop");
        let w = Wsdl::parse(&text, None).unwrap();
        let op = w.operation(None, None, "order").unwrap();
        assert!(op.wrapped());
        assert_eq!(op.input.header_parts.len(), 1);
        assert_eq!(op.faults.len(), 1);
        assert!(w.operation(None, None, "ping").unwrap().output.is_none());
    }
}
