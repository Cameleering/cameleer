//! A small embedded FHIR R4 type registry: element names, types and cardinality of the
//! common data types and of `Patient`, `Practitioner`, `Organization`, `Observation`,
//! `Encounter`, `Bundle`, `OperationOutcome`, `CapabilityStatement` and `Parameters` — enough
//! to decide, when converting between JSON and XML, which elements repeat (JSON arrays),
//! which are primitives (and their JSON type: boolean, number or string), where resources
//! are nested and where XHTML is embedded.
//!
//! Elements of other resources are handled heuristically (an element with a `value`
//! attribute is a primitive string; repeated siblings and a few well-known names such as
//! `extension`, `identifier`, `coding` are arrays).

use std::collections::HashMap;
use std::sync::OnceLock;

/// The FHIR namespace.
pub const FHIR_NS: &str = "http://hl7.org/fhir";
/// The XHTML namespace (narratives).
pub const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";

const DEFS: &str = r#"
Extension: url uri, value[x] *
Narrative: status code, div xhtml
Meta: versionId id, lastUpdated instant, source uri, profile canonical*, security Coding*, tag Coding*
Identifier: use code, type CodeableConcept, system uri, value string, period Period, assigner Reference
HumanName: use code, text string, family string, given string*, prefix string*, suffix string*, period Period
ContactPoint: system code, value string, use code, rank positiveInt, period Period
Address: use code, type code, text string, line string*, city string, district string, state string, postalCode string, country string, period Period
CodeableConcept: coding Coding*, text string
Coding: system uri, version string, code code, display string, userSelected boolean
Reference: reference string, type uri, identifier Identifier, display string
Period: start dateTime, end dateTime
Quantity: value decimal, comparator code, unit string, system uri, code code
Range: low Quantity, high Quantity
Ratio: numerator Quantity, denominator Quantity
SampledData: origin Quantity, period decimal, factor decimal, lowerLimit decimal, upperLimit decimal, dimensions positiveInt, data string
Attachment: contentType code, language code, data base64Binary, url url, size unsignedInt, hash base64Binary, title string, creation dateTime
Annotation: author[x] Reference|string, time dateTime, text markdown
Timing: event dateTime*, repeat Timing.repeat, code CodeableConcept
Timing.repeat: bounds[x] Duration|Range|Period, count positiveInt, countMax positiveInt, duration decimal, durationMax decimal, durationUnit code, frequency positiveInt, frequencyMax positiveInt, period decimal, periodMax decimal, periodUnit code, dayOfWeek code*, timeOfDay time*, when code*, offset unsignedInt
Money: value decimal, currency code
Signature: type Coding*, when instant, who Reference, onBehalfOf Reference, targetFormat code, sigFormat code, data base64Binary
ContactDetail: name string, telecom ContactPoint*
UsageContext: code Coding, value[x] CodeableConcept|Quantity|Range|Reference
Resource: id id, meta Meta, implicitRules uri, language code
DomainResource: text Narrative, contained Resource*, extension Extension*, modifierExtension Extension*
Patient: identifier Identifier*, active boolean, name HumanName*, telecom ContactPoint*, gender code, birthDate date, deceased[x] boolean|dateTime, address Address*, maritalStatus CodeableConcept, multipleBirth[x] boolean|integer, photo Attachment*, contact Patient.contact*, communication Patient.communication*, generalPractitioner Reference*, managingOrganization Reference, link Patient.link*
Patient.contact: relationship CodeableConcept*, name HumanName, telecom ContactPoint*, address Address, gender code, organization Reference, period Period
Patient.communication: language CodeableConcept, preferred boolean
Patient.link: other Reference, type code
Practitioner: identifier Identifier*, active boolean, name HumanName*, telecom ContactPoint*, address Address*, gender code, birthDate date, photo Attachment*, qualification Practitioner.qualification*, communication CodeableConcept*
Practitioner.qualification: identifier Identifier*, code CodeableConcept, period Period, issuer Reference
Organization: identifier Identifier*, active boolean, type CodeableConcept*, name string, alias string*, telecom ContactPoint*, address Address*, partOf Reference, contact Organization.contact*, endpoint Reference*
Organization.contact: purpose CodeableConcept, name HumanName, telecom ContactPoint*, address Address
Observation: identifier Identifier*, basedOn Reference*, partOf Reference*, status code, category CodeableConcept*, code CodeableConcept, subject Reference, focus Reference*, encounter Reference, effective[x] dateTime|Period|Timing|instant, issued instant, performer Reference*, value[x] Quantity|CodeableConcept|string|boolean|integer|Range|Ratio|SampledData|time|dateTime|Period, dataAbsentReason CodeableConcept, interpretation CodeableConcept*, note Annotation*, bodySite CodeableConcept, method CodeableConcept, specimen Reference, device Reference, referenceRange Observation.referenceRange*, hasMember Reference*, derivedFrom Reference*, component Observation.component*
Observation.referenceRange: low Quantity, high Quantity, type CodeableConcept, appliesTo CodeableConcept*, age Range, text string
Observation.component: code CodeableConcept, value[x] Quantity|CodeableConcept|string|boolean|integer|Range|Ratio|SampledData|time|dateTime|Period, dataAbsentReason CodeableConcept, interpretation CodeableConcept*, referenceRange Observation.referenceRange*
Encounter: identifier Identifier*, status code, statusHistory Encounter.statusHistory*, class Coding, classHistory Encounter.classHistory*, type CodeableConcept*, serviceType CodeableConcept, priority CodeableConcept, subject Reference, episodeOfCare Reference*, basedOn Reference*, participant Encounter.participant*, appointment Reference*, period Period, length Duration, reasonCode CodeableConcept*, reasonReference Reference*, diagnosis Encounter.diagnosis*, account Reference*, hospitalization Encounter.hospitalization, location Encounter.location*, serviceProvider Reference, partOf Reference
Encounter.statusHistory: status code, period Period
Encounter.classHistory: class Coding, period Period
Encounter.participant: type CodeableConcept*, period Period, individual Reference
Encounter.diagnosis: condition Reference, use CodeableConcept, rank positiveInt
Encounter.hospitalization: preAdmissionIdentifier Identifier, origin Reference, admitSource CodeableConcept, reAdmission CodeableConcept, dietPreference CodeableConcept*, specialCourtesy CodeableConcept*, specialArrangement CodeableConcept*, destination Reference, dischargeDisposition CodeableConcept
Encounter.location: location Reference, status code, physicalType CodeableConcept, period Period
Bundle: identifier Identifier, type code, timestamp instant, total unsignedInt, link Bundle.link*, entry Bundle.entry*, signature Signature
Bundle.link: relation string, url uri
Bundle.entry: link Bundle.link*, fullUrl uri, resource Resource, search Bundle.entry.search, request Bundle.entry.request, response Bundle.entry.response
Bundle.entry.search: mode code, score decimal
Bundle.entry.request: method code, url uri, ifNoneMatch string, ifModifiedSince instant, ifMatch string, ifNoneExist string
Bundle.entry.response: status string, location uri, etag string, lastModified instant, outcome Resource
OperationOutcome: issue OperationOutcome.issue*
OperationOutcome.issue: severity code, code code, details CodeableConcept, diagnostics string, location string*, expression string*
CapabilityStatement: url uri, version string, name string, title string, status code, experimental boolean, date dateTime, publisher string, contact ContactDetail*, description markdown, kind code, instantiates canonical*, imports canonical*, software CapabilityStatement.software, implementation CapabilityStatement.implementation, fhirVersion code, format code*, patchFormat code*, implementationGuide canonical*, rest CapabilityStatement.rest*
CapabilityStatement.software: name string, version string, releaseDate dateTime
CapabilityStatement.implementation: description string, url url
CapabilityStatement.rest: mode code, documentation markdown, resource CapabilityStatement.rest.resource*, interaction CapabilityStatement.rest.interaction*
CapabilityStatement.rest.resource: type code, profile canonical, interaction CapabilityStatement.rest.resource.interaction*, versioning code, readHistory boolean, updateCreate boolean, searchParam CapabilityStatement.rest.resource.searchParam*
CapabilityStatement.rest.resource.interaction: code code, documentation markdown
CapabilityStatement.rest.interaction: code code, documentation markdown
CapabilityStatement.rest.resource.searchParam: name string, definition canonical, type code, documentation markdown
Parameters: parameter Parameters.parameter*
Parameters.parameter: name string, value[x] *, resource Resource, part Parameters.parameter*
"#;

/// FHIR primitive types.
pub const PRIMITIVES: &[&str] = &[
    "boolean", "integer", "integer64", "decimal", "positiveInt", "unsignedInt", "string", "uri", "url", "canonical", "code", "oid", "id", "uuid", "markdown", "base64Binary", "date",
    "dateTime", "instant", "time", "xhtml",
];

/// The resources that are not `DomainResource`s.
const PLAIN_RESOURCES: &[&str] = &["Bundle", "Parameters", "Binary"];

/// Whether a type name is a primitive.
pub fn is_primitive(t: &str) -> bool {
    PRIMITIVES.contains(&t)
}

/// How a primitive is represented in JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonKind {
    Bool,
    Number,
    String,
}

/// JSON representation of a primitive type.
pub fn json_kind(t: &str) -> JsonKind {
    match t {
        "boolean" => JsonKind::Bool,
        "integer" | "decimal" | "positiveInt" | "unsignedInt" => JsonKind::Number,
        _ => JsonKind::String,
    }
}

#[derive(Clone, Debug)]
struct Def {
    name: String,
    ty: String,
    many: bool,
    /// For `name[x]`: allowed types (empty = any).
    choices: Option<Vec<String>>,
}

fn registry() -> &'static HashMap<String, Vec<Def>> {
    static R: OnceLock<HashMap<String, Vec<Def>>> = OnceLock::new();
    R.get_or_init(|| {
        let mut m = HashMap::new();
        for line in DEFS.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let (owner, rest) = line.split_once(':').unwrap_or((line, ""));
            let defs = rest
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|e| {
                    let (n, t) = e.split_once(' ').unwrap_or((e, "string"));
                    let many = t.ends_with('*') && t != "*";
                    let t = if t == "*" { "*" } else { t.trim_end_matches('*') };
                    match n.strip_suffix("[x]") {
                        Some(base) => Def { name: base.to_string(), ty: "choice".into(), many, choices: Some(if t == "*" { Vec::new() } else { t.split('|').map(str::to_string).collect() }) },
                        None => Def { name: n.to_string(), ty: t.to_string(), many, choices: None },
                    }
                })
                .collect();
            m.insert(owner.trim().to_string(), defs);
        }
        m
    })
}

/// Whether a type is a known resource type.
pub fn is_resource(t: &str) -> bool {
    matches!(t, "Resource" | "DomainResource" | "Patient" | "Practitioner" | "Organization" | "Observation" | "Encounter" | "Bundle" | "OperationOutcome" | "CapabilityStatement" | "Parameters")
}

fn alias(t: &str) -> &str {
    match t {
        "Age" | "Duration" | "Distance" | "Count" | "SimpleQuantity" | "MoneyQuantity" => "Quantity",
        other => other,
    }
}

fn choice_type(suffix: &str) -> String {
    let lower = {
        let mut c = suffix.chars();
        c.next().map(|f| f.to_lowercase().collect::<String>() + c.as_str()).unwrap_or_default()
    };
    if is_primitive(&lower) {
        lower
    } else if suffix == "Base64Binary" {
        "base64Binary".into()
    } else {
        suffix.to_string()
    }
}

fn lookup_in(owner: &str, name: &str) -> Option<(String, bool)> {
    let defs = registry().get(alias(owner))?;
    if let Some(d) = defs.iter().find(|d| d.name == name && d.choices.is_none()) {
        return Some((d.ty.clone(), d.many));
    }
    for d in defs.iter().filter(|d| d.choices.is_some()) {
        if let Some(rest) = name.strip_prefix(&d.name) {
            if rest.chars().next().map(char::is_uppercase).unwrap_or(false) {
                let t = choice_type(rest);
                let allowed = d.choices.as_ref().map(|c| c.is_empty() || c.iter().any(|x| choice_type(x) == t || x == rest)).unwrap_or(true);
                if allowed {
                    return Some((t, d.many));
                }
            }
        }
    }
    None
}

/// The type and repetition of element `name` of type `owner` (a type name such as
/// `Patient`, `HumanName` or a backbone path such as `Bundle.entry`). `None` when unknown.
/// `resource` tells whether `owner` is a resource (also for resource types missing from the
/// registry, which then get the `Resource` / `DomainResource` elements).
pub fn element(owner: &str, name: &str, resource: bool) -> Option<(String, bool)> {
    if let Some(r) = lookup_in(owner, name) {
        return Some(r);
    }
    if resource || is_resource(owner) {
        if let Some(r) = lookup_in("Resource", name) {
            return Some(r);
        }
        if !PLAIN_RESOURCES.contains(&owner) {
            if let Some(r) = lookup_in("DomainResource", name) {
                return Some(r);
            }
        }
    }
    match name {
        "extension" => Some(("Extension".into(), true)),
        "modifierExtension" if owner.contains('.') || resource => Some(("Extension".into(), true)),
        _ => None,
    }
}

/// Names that repeat in practically every resource (used for unknown types).
pub fn usually_repeats(name: &str) -> bool {
    matches!(name, "extension" | "modifierExtension" | "identifier" | "coding" | "contained" | "telecom" | "given" | "prefix" | "suffix" | "line" | "category" | "performer" | "note" | "entry" | "issue" | "component" | "contact" | "link" | "profile" | "tag" | "security" | "address")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        assert_eq!(element("Patient", "name", true), Some(("HumanName".into(), true)));
        assert_eq!(element("Patient", "birthDate", true), Some(("date".into(), false)));
        assert_eq!(element("Patient", "deceasedBoolean", true), Some(("boolean".into(), false)));
        assert_eq!(element("Patient", "id", true), Some(("id".into(), false)));
        assert_eq!(element("Patient", "contained", true), Some(("Resource".into(), true)));
        assert_eq!(element("Bundle", "text", true), None);
        assert_eq!(element("Observation", "valueQuantity", true), Some(("Quantity".into(), false)));
        assert_eq!(element("Extension", "valueDecimal", false), Some(("decimal".into(), false)));
        assert_eq!(element("HumanName", "given", false), Some(("string".into(), true)));
        assert_eq!(element("Bundle.entry", "resource", false), Some(("Resource".into(), false)));
        assert_eq!(element("Age", "value", false), Some(("decimal".into(), false)));
        assert!(is_resource("Patient") && !is_resource("HumanName"));
    }
}
