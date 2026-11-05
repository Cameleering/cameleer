//! Test fixtures: a document/literal wrapped calculator WSDL (SOAP 1.1 and 1.2 ports), the
//! classic rpc/literal stock quote WSDL (after the WSDL 1.1 specification example), sample
//! envelopes, and the embedded FHIR mock server ([`crate::fhir::testing::FhirMock`]).

pub use crate::fhir::testing::FhirMock;

/// Target namespace of [`CALCULATOR_WSDL`].
pub const CALC_NS: &str = "http://example.org/calculator";

/// A document/literal wrapped calculator service with `Add`, `Divide` (declared fault
/// `DivideByZeroFault`) and the one-way `Ping`, bound to SOAP 1.1 (`CalculatorSoap`) and
/// SOAP 1.2 (`CalculatorSoap12`) ports of `CalculatorService`.
pub const CALCULATOR_WSDL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<wsdl:definitions name="Calculator" targetNamespace="http://example.org/calculator"
    xmlns:wsdl="http://schemas.xmlsoap.org/wsdl/" xmlns:soap="http://schemas.xmlsoap.org/wsdl/soap/"
    xmlns:soap12="http://schemas.xmlsoap.org/wsdl/soap12/" xmlns:xs="http://www.w3.org/2001/XMLSchema"
    xmlns:tns="http://example.org/calculator">
  <wsdl:types>
    <xs:schema targetNamespace="http://example.org/calculator" elementFormDefault="qualified">
      <xs:element name="Add"><xs:complexType><xs:sequence>
        <xs:element name="a" type="xs:int"/><xs:element name="b" type="xs:int"/>
      </xs:sequence></xs:complexType></xs:element>
      <xs:element name="AddResponse"><xs:complexType><xs:sequence>
        <xs:element name="AddResult" type="xs:int"/>
      </xs:sequence></xs:complexType></xs:element>
      <xs:element name="Divide"><xs:complexType><xs:sequence>
        <xs:element name="dividend" type="xs:double"/><xs:element name="divisor" type="xs:double"/>
      </xs:sequence></xs:complexType></xs:element>
      <xs:element name="DivideResponse"><xs:complexType><xs:sequence>
        <xs:element name="DivideResult" type="xs:double"/>
      </xs:sequence></xs:complexType></xs:element>
      <xs:element name="DivideByZeroFault"><xs:complexType><xs:sequence>
        <xs:element name="message" type="xs:string"/><xs:element name="dividend" type="xs:double"/>
      </xs:sequence></xs:complexType></xs:element>
      <xs:element name="Ping"><xs:complexType><xs:sequence>
        <xs:element name="message" type="xs:string"/>
      </xs:sequence></xs:complexType></xs:element>
    </xs:schema>
  </wsdl:types>
  <wsdl:message name="AddRequest"><wsdl:part name="parameters" element="tns:Add"/></wsdl:message>
  <wsdl:message name="AddResponse"><wsdl:part name="parameters" element="tns:AddResponse"/></wsdl:message>
  <wsdl:message name="DivideRequest"><wsdl:part name="parameters" element="tns:Divide"/></wsdl:message>
  <wsdl:message name="DivideResponse"><wsdl:part name="parameters" element="tns:DivideResponse"/></wsdl:message>
  <wsdl:message name="DivideByZero"><wsdl:part name="fault" element="tns:DivideByZeroFault"/></wsdl:message>
  <wsdl:message name="PingRequest"><wsdl:part name="parameters" element="tns:Ping"/></wsdl:message>
  <wsdl:portType name="CalculatorPortType">
    <wsdl:operation name="Add"><wsdl:input message="tns:AddRequest"/><wsdl:output message="tns:AddResponse"/></wsdl:operation>
    <wsdl:operation name="Divide"><wsdl:input message="tns:DivideRequest"/><wsdl:output message="tns:DivideResponse"/>
      <wsdl:fault name="DivideByZero" message="tns:DivideByZero"/></wsdl:operation>
    <wsdl:operation name="Ping"><wsdl:input message="tns:PingRequest"/></wsdl:operation>
  </wsdl:portType>
  <wsdl:binding name="CalculatorSoap11Binding" type="tns:CalculatorPortType">
    <soap:binding style="document" transport="http://schemas.xmlsoap.org/soap/http"/>
    <wsdl:operation name="Add"><soap:operation soapAction="http://example.org/calculator/Add"/>
      <wsdl:input><soap:body use="literal"/></wsdl:input><wsdl:output><soap:body use="literal"/></wsdl:output></wsdl:operation>
    <wsdl:operation name="Divide"><soap:operation soapAction="http://example.org/calculator/Divide"/>
      <wsdl:input><soap:body use="literal"/></wsdl:input><wsdl:output><soap:body use="literal"/></wsdl:output>
      <wsdl:fault name="DivideByZero"><soap:fault name="DivideByZero" use="literal"/></wsdl:fault></wsdl:operation>
    <wsdl:operation name="Ping"><soap:operation soapAction="http://example.org/calculator/Ping"/>
      <wsdl:input><soap:body use="literal"/></wsdl:input></wsdl:operation>
  </wsdl:binding>
  <wsdl:binding name="CalculatorSoap12Binding" type="tns:CalculatorPortType">
    <soap12:binding style="document" transport="http://schemas.xmlsoap.org/soap/http"/>
    <wsdl:operation name="Add"><soap12:operation soapAction="http://example.org/calculator/Add"/>
      <wsdl:input><soap12:body use="literal"/></wsdl:input><wsdl:output><soap12:body use="literal"/></wsdl:output></wsdl:operation>
    <wsdl:operation name="Divide"><soap12:operation soapAction="http://example.org/calculator/Divide"/>
      <wsdl:input><soap12:body use="literal"/></wsdl:input><wsdl:output><soap12:body use="literal"/></wsdl:output>
      <wsdl:fault name="DivideByZero"><soap12:fault name="DivideByZero" use="literal"/></wsdl:fault></wsdl:operation>
    <wsdl:operation name="Ping"><soap12:operation soapAction="http://example.org/calculator/Ping"/>
      <wsdl:input><soap12:body use="literal"/></wsdl:input></wsdl:operation>
  </wsdl:binding>
  <wsdl:service name="CalculatorService">
    <wsdl:port name="CalculatorSoap" binding="tns:CalculatorSoap11Binding"><soap:address location="http://localhost:9000/calculator"/></wsdl:port>
    <wsdl:port name="CalculatorSoap12" binding="tns:CalculatorSoap12Binding"><soap12:address location="http://localhost:9000/calculator12"/></wsdl:port>
  </wsdl:service>
</wsdl:definitions>
"#;

/// The rpc/literal stock quote service (`GetLastTradePrice(tickerSymbol: string) -> float`),
/// after example 4 of the WSDL 1.1 specification.
pub const STOCK_RPC_WSDL: &str = r#"<?xml version="1.0"?>
<definitions name="StockQuote" targetNamespace="http://example.com/stockquote.wsdl"
    xmlns:tns="http://example.com/stockquote.wsdl" xmlns:xsd="http://www.w3.org/2001/XMLSchema"
    xmlns:soap="http://schemas.xmlsoap.org/wsdl/soap/" xmlns="http://schemas.xmlsoap.org/wsdl/">
  <message name="GetLastTradePriceInput"><part name="tickerSymbol" type="xsd:string"/></message>
  <message name="GetLastTradePriceOutput"><part name="result" type="xsd:float"/></message>
  <portType name="StockQuotePortType">
    <operation name="GetLastTradePrice"><input message="tns:GetLastTradePriceInput"/><output message="tns:GetLastTradePriceOutput"/></operation>
  </portType>
  <binding name="StockQuoteSoapBinding" type="tns:StockQuotePortType">
    <soap:binding style="rpc" transport="http://schemas.xmlsoap.org/soap/http"/>
    <operation name="GetLastTradePrice">
      <soap:operation soapAction="http://example.com/GetLastTradePrice"/>
      <input><soap:body use="literal" namespace="urn:example:stock"/></input>
      <output><soap:body use="literal" namespace="urn:example:stock"/></output>
    </operation>
  </binding>
  <service name="StockQuoteService">
    <documentation>My first service</documentation>
    <port name="StockQuotePort" binding="tns:StockQuoteSoapBinding"><soap:address location="http://example.com/stockquote"/></port>
  </service>
</definitions>
"#;

/// A SOAP 1.1 request for `Add(2, 3)` as a .NET / WS-I Basic Profile client sends it.
pub const ADD_REQUEST_11: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <Add xmlns="http://example.org/calculator">
      <a>2</a>
      <b>3</b>
    </Add>
  </soap:Body>
</soap:Envelope>"#;

/// A hand-written SOAP 1.2 fault with a subcode and an application detail.
pub const SOAP12_FAULT: &str = r#"<env:Envelope xmlns:env="http://www.w3.org/2003/05/soap-envelope" xmlns:m="http://example.org/calculator">
  <env:Body>
    <env:Fault>
      <env:Code><env:Value>env:Sender</env:Value><env:Subcode><env:Value>m:DivideByZero</env:Value></env:Subcode></env:Code>
      <env:Reason><env:Text xml:lang="en">Division by zero</env:Text><env:Text xml:lang="fr">Division par zéro</env:Text></env:Reason>
      <env:Detail><m:DivideByZeroFault><m:message>Division by zero</m:message><m:dividend>1</m:dividend></m:DivideByZeroFault></env:Detail>
    </env:Fault>
  </env:Body>
</env:Envelope>"#;
