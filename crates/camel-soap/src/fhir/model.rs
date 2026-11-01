//! A small embedded FHIR type registry (R4, with DSTU3 and R5 overrides for the resources that changed): element names, types, cardinality and spec order of the
//! common data types and of the resources in [`RESOURCES`] (Patient, Practitioner, Organization, Observation,
//! Encounter, Condition, Procedure, MedicationRequest, StructureDefinition...) — enough
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
Dosage: sequence integer, text string, additionalInstruction CodeableConcept*, patientInstruction string, timing Timing, asNeeded[x] boolean|CodeableConcept, site CodeableConcept, route CodeableConcept, method CodeableConcept, doseAndRate Dosage.doseAndRate*, maxDosePerPeriod Ratio, maxDosePerAdministration Quantity, maxDosePerLifetime Quantity
Dosage.doseAndRate: type CodeableConcept, dose[x] Range|Quantity, rate[x] Ratio|Range|Quantity
Condition: identifier Identifier*, clinicalStatus CodeableConcept, verificationStatus CodeableConcept, category CodeableConcept*, severity CodeableConcept, code CodeableConcept, bodySite CodeableConcept*, subject Reference, encounter Reference, onset[x] dateTime|Age|Period|Range|string, abatement[x] dateTime|Age|Period|Range|string, recordedDate dateTime, recorder Reference, asserter Reference, stage Condition.stage*, evidence Condition.evidence*, note Annotation*
Condition.stage: summary CodeableConcept, assessment Reference*, type CodeableConcept
Condition.evidence: code CodeableConcept*, detail Reference*
Procedure: identifier Identifier*, instantiatesCanonical canonical*, instantiatesUri uri*, basedOn Reference*, partOf Reference*, status code, statusReason CodeableConcept, category CodeableConcept, code CodeableConcept, subject Reference, encounter Reference, performed[x] dateTime|Period|string|Age|Range, recorder Reference, asserter Reference, performer Procedure.performer*, location Reference, reasonCode CodeableConcept*, reasonReference Reference*, bodySite CodeableConcept*, outcome CodeableConcept, report Reference*, complication CodeableConcept*, complicationDetail Reference*, followUp CodeableConcept*, note Annotation*, focalDevice Procedure.focalDevice*, usedReference Reference*, usedCode CodeableConcept*
Procedure.performer: function CodeableConcept, actor Reference, onBehalfOf Reference
Procedure.focalDevice: action CodeableConcept, manipulated Reference
Medication: identifier Identifier*, code CodeableConcept, status code, manufacturer Reference, form CodeableConcept, amount Ratio, ingredient Medication.ingredient*, batch Medication.batch
Medication.ingredient: item[x] CodeableConcept|Reference, isActive boolean, strength Ratio
Medication.batch: lotNumber string, expirationDate dateTime
MedicationRequest: identifier Identifier*, status code, statusReason CodeableConcept, intent code, category CodeableConcept*, priority code, doNotPerform boolean, reported[x] boolean|Reference, medication[x] CodeableConcept|Reference, subject Reference, encounter Reference, supportingInformation Reference*, authoredOn dateTime, requester Reference, performer Reference, performerType CodeableConcept, recorder Reference, reasonCode CodeableConcept*, reasonReference Reference*, instantiatesCanonical canonical*, instantiatesUri uri*, basedOn Reference*, groupIdentifier Identifier, courseOfTherapyType CodeableConcept, insurance Reference*, note Annotation*, dosageInstruction Dosage*, dispenseRequest MedicationRequest.dispenseRequest, substitution MedicationRequest.substitution, priorPrescription Reference, detectedIssue Reference*, eventHistory Reference*
MedicationRequest.dispenseRequest: initialFill MedicationRequest.dispenseRequest.initialFill, dispenseInterval Duration, validityPeriod Period, numberOfRepeatsAllowed unsignedInt, quantity Quantity, expectedSupplyDuration Duration, performer Reference
MedicationRequest.dispenseRequest.initialFill: quantity Quantity, duration Duration
MedicationRequest.substitution: allowed[x] boolean|CodeableConcept, reason CodeableConcept
AllergyIntolerance: identifier Identifier*, clinicalStatus CodeableConcept, verificationStatus CodeableConcept, type code, category code*, criticality code, code CodeableConcept, patient Reference, encounter Reference, onset[x] dateTime|Age|Period|Range|string, recordedDate dateTime, recorder Reference, asserter Reference, lastOccurrence dateTime, note Annotation*, reaction AllergyIntolerance.reaction*
AllergyIntolerance.reaction: substance CodeableConcept, manifestation CodeableConcept*, description string, onset dateTime, severity code, exposureRoute CodeableConcept, note Annotation*
DiagnosticReport: identifier Identifier*, basedOn Reference*, status code, category CodeableConcept*, code CodeableConcept, subject Reference, encounter Reference, effective[x] dateTime|Period, issued instant, performer Reference*, resultsInterpreter Reference*, specimen Reference*, result Reference*, imagingStudy Reference*, media DiagnosticReport.media*, conclusion string, conclusionCode CodeableConcept*, presentedForm Attachment*
DiagnosticReport.media: comment string, link Reference
Immunization: identifier Identifier*, status code, statusReason CodeableConcept, vaccineCode CodeableConcept, patient Reference, encounter Reference, occurrence[x] dateTime|string, recorded dateTime, primarySource boolean, reportOrigin CodeableConcept, location Reference, manufacturer Reference, lotNumber string, expirationDate date, site CodeableConcept, route CodeableConcept, doseQuantity Quantity, performer Immunization.performer*, note Annotation*, reasonCode CodeableConcept*, reasonReference Reference*, isSubpotent boolean
Immunization.performer: function CodeableConcept, actor Reference
Location: identifier Identifier*, status code, operationalStatus Coding, name string, alias string*, description string, mode code, type CodeableConcept*, telecom ContactPoint*, address Address, physicalType CodeableConcept, position Location.position, managingOrganization Reference, partOf Reference, hoursOfOperation Location.hoursOfOperation*, availabilityExceptions string, endpoint Reference*
Location.position: longitude decimal, latitude decimal, altitude decimal
Location.hoursOfOperation: daysOfWeek code*, allDay boolean, openingTime time, closingTime time
Device: identifier Identifier*, definition Reference, status code, statusReason CodeableConcept*, distinctIdentifier string, manufacturer string, manufactureDate dateTime, expirationDate dateTime, lotNumber string, serialNumber string, modelNumber string, partNumber string, type CodeableConcept, patient Reference, owner Reference, contact ContactPoint*, location Reference, url uri, note Annotation*, safety CodeableConcept*, parent Reference
ServiceRequest: identifier Identifier*, instantiatesCanonical canonical*, instantiatesUri uri*, basedOn Reference*, replaces Reference*, requisition Identifier, status code, intent code, category CodeableConcept*, priority code, doNotPerform boolean, code CodeableConcept, orderDetail CodeableConcept*, quantity[x] Quantity|Ratio|Range, subject Reference, encounter Reference, occurrence[x] dateTime|Period|Timing, asNeeded[x] boolean|CodeableConcept, authoredOn dateTime, requester Reference, performerType CodeableConcept, performer Reference*, locationCode CodeableConcept*, locationReference Reference*, reasonCode CodeableConcept*, reasonReference Reference*, insurance Reference*, supportingInfo Reference*, specimen Reference*, bodySite CodeableConcept*, note Annotation*, patientInstruction string, relevantHistory Reference*
Appointment: identifier Identifier*, status code, cancelationReason CodeableConcept, serviceCategory CodeableConcept*, serviceType CodeableConcept*, specialty CodeableConcept*, appointmentType CodeableConcept, reasonCode CodeableConcept*, reasonReference Reference*, priority unsignedInt, description string, supportingInformation Reference*, start instant, end instant, minutesDuration positiveInt, slot Reference*, created dateTime, comment string, patientInstruction string, basedOn Reference*, participant Appointment.participant*, requestedPeriod Period*
Appointment.participant: type CodeableConcept*, actor Reference, required code, status code, period Period
Binary: contentType code, securityContext Reference, data base64Binary
Basic: identifier Identifier*, code CodeableConcept, subject Reference, created date, author Reference
StructureDefinition: url uri, identifier Identifier*, version string, name string, title string, status code, experimental boolean, date dateTime, publisher string, contact ContactDetail*, description markdown, useContext UsageContext*, jurisdiction CodeableConcept*, purpose markdown, copyright markdown, keyword Coding*, fhirVersion code, mapping StructureDefinition.mapping*, kind code, abstract boolean, context StructureDefinition.context*, contextInvariant string*, type uri, baseDefinition canonical, derivation code, snapshot StructureDefinition.snapshot, differential StructureDefinition.differential
StructureDefinition.mapping: identity id, uri uri, name string, comment string
StructureDefinition.context: type code, expression string
StructureDefinition.snapshot: element ElementDefinition*
StructureDefinition.differential: element ElementDefinition*
ElementDefinition: path string, representation code*, sliceName string, sliceIsConstraining boolean, label string, code Coding*, slicing ElementDefinition.slicing, short string, definition markdown, comment markdown, requirements markdown, alias string*, min unsignedInt, max string, base ElementDefinition.base, contentReference uri, type ElementDefinition.type*, defaultValue[x] *, meaningWhenMissing markdown, orderMeaning string, fixed[x] *, pattern[x] *, example ElementDefinition.example*, minValue[x] date|dateTime|instant|time|decimal|integer|positiveInt|unsignedInt|Quantity, maxValue[x] date|dateTime|instant|time|decimal|integer|positiveInt|unsignedInt|Quantity, maxLength integer, condition id*, constraint ElementDefinition.constraint*, mustSupport boolean, isModifier boolean, isModifierReason string, isSummary boolean, binding ElementDefinition.binding, mapping ElementDefinition.mapping*
ElementDefinition.slicing: discriminator ElementDefinition.slicing.discriminator*, description string, ordered boolean, rules code
ElementDefinition.slicing.discriminator: type code, path string
ElementDefinition.base: path string, min unsignedInt, max string
ElementDefinition.type: code uri, profile canonical*, targetProfile canonical*, aggregation code*, versioning code
ElementDefinition.example: label string, value[x] *
ElementDefinition.constraint: key id, requirements string, severity code, human string, expression string, xpath string, source canonical
ElementDefinition.binding: strength code, description string, valueSet canonical
ElementDefinition.mapping: identity id, language code, map string, comment string
@DSTU3 Observation: identifier Identifier*, basedOn Reference*, status code, category CodeableConcept*, code CodeableConcept, subject Reference, context Reference, effective[x] dateTime|Period, issued instant, performer Reference*, value[x] Quantity|CodeableConcept|string|boolean|Range|Ratio|SampledData|Attachment|time|dateTime|Period, dataAbsentReason CodeableConcept, interpretation CodeableConcept, comment string, bodySite CodeableConcept, method CodeableConcept, specimen Reference, device Reference, referenceRange Observation.referenceRange*, related Observation.related*, component Observation.component*
@DSTU3 Observation.related: type code, target Reference
@DSTU3 Observation.component: code CodeableConcept, value[x] Quantity|CodeableConcept|string|Range|Ratio|SampledData|Attachment|time|dateTime|Period, dataAbsentReason CodeableConcept, interpretation CodeableConcept, referenceRange Observation.referenceRange*
@DSTU3 Encounter: identifier Identifier*, status code, statusHistory Encounter.statusHistory*, class Coding, classHistory Encounter.classHistory*, type CodeableConcept*, priority CodeableConcept, subject Reference, episodeOfCare Reference*, incomingReferral Reference*, participant Encounter.participant*, appointment Reference, period Period, length Duration, reason CodeableConcept*, diagnosis Encounter.diagnosis*, account Reference*, hospitalization Encounter.hospitalization, location Encounter.location*, serviceProvider Reference, partOf Reference
@DSTU3 Encounter.diagnosis: condition Reference, role CodeableConcept, rank positiveInt
@DSTU3 Patient: identifier Identifier*, active boolean, name HumanName*, telecom ContactPoint*, gender code, birthDate date, deceased[x] boolean|dateTime, address Address*, maritalStatus CodeableConcept, multipleBirth[x] boolean|integer, photo Attachment*, contact Patient.contact*, animal Patient.animal, communication Patient.communication*, generalPractitioner Reference*, managingOrganization Reference, link Patient.link*
@DSTU3 Patient.animal: species CodeableConcept, breed CodeableConcept, genderStatus CodeableConcept
@DSTU3 Condition: identifier Identifier*, clinicalStatus code, verificationStatus code, category CodeableConcept*, severity CodeableConcept, code CodeableConcept, bodySite CodeableConcept*, subject Reference, context Reference, onset[x] dateTime|Age|Period|Range|string, abatement[x] dateTime|Age|boolean|Period|Range|string, assertedDate dateTime, asserter Reference, stage Condition.stage, evidence Condition.evidence*, note Annotation*
@DSTU3 Condition.stage: summary CodeableConcept, assessment Reference*
@DSTU3 AllergyIntolerance: identifier Identifier*, clinicalStatus code, verificationStatus code, type code, category code*, criticality code, code CodeableConcept, patient Reference, onset[x] dateTime|Age|Period|Range|string, assertedDate dateTime, recorder Reference, asserter Reference, lastOccurrence dateTime, note Annotation*, reaction AllergyIntolerance.reaction*
@DSTU3 MedicationRequest: identifier Identifier*, definition Reference*, basedOn Reference*, groupIdentifier Identifier, status code, intent code, category CodeableConcept, priority code, medication[x] CodeableConcept|Reference, subject Reference, context Reference, supportingInformation Reference*, authoredOn dateTime, requester MedicationRequest.requester, recorder Reference, reasonCode CodeableConcept*, reasonReference Reference*, note Annotation*, dosageInstruction Dosage*, dispenseRequest MedicationRequest.dispenseRequest, substitution MedicationRequest.substitution, priorPrescription Reference, detectedIssue Reference*, eventHistory Reference*
@DSTU3 MedicationRequest.requester: agent Reference, onBehalfOf Reference
@DSTU3 MedicationRequest.substitution: allowed boolean, reason CodeableConcept
@DSTU3 Dosage: sequence integer, text string, additionalInstruction CodeableConcept*, patientInstruction string, timing Timing, asNeeded[x] boolean|CodeableConcept, site CodeableConcept, route CodeableConcept, method CodeableConcept, dose[x] Range|Quantity, maxDosePerPeriod Ratio, maxDosePerAdministration Quantity, maxDosePerLifetime Quantity, rate[x] Ratio|Range|Quantity
@R5 CodeableReference: concept CodeableConcept, reference Reference
@R5 Observation: identifier Identifier*, instantiates[x] canonical|Reference, basedOn Reference*, triggeredBy Observation.triggeredBy*, partOf Reference*, status code, category CodeableConcept*, code CodeableConcept, subject Reference, focus Reference*, encounter Reference, effective[x] dateTime|Period|Timing|instant, issued instant, performer Reference*, value[x] Quantity|CodeableConcept|string|boolean|integer|Range|Ratio|SampledData|time|dateTime|Period|Attachment|Reference, dataAbsentReason CodeableConcept, interpretation CodeableConcept*, note Annotation*, bodySite CodeableConcept, bodyStructure Reference, method CodeableConcept, specimen Reference, device Reference, referenceRange Observation.referenceRange*, hasMember Reference*, derivedFrom Reference*, component Observation.component*
@R5 Observation.triggeredBy: observation Reference, type code, reason string
@R5 Encounter: identifier Identifier*, status code, class CodeableConcept*, priority CodeableConcept, type CodeableConcept*, serviceType CodeableReference*, subject Reference, subjectStatus CodeableConcept, episodeOfCare Reference*, basedOn Reference*, careTeam Reference*, partOf Reference, serviceProvider Reference, participant Encounter.participant*, appointment Reference*, virtualService VirtualServiceDetail*, actualPeriod Period, plannedStartDate dateTime, plannedEndDate dateTime, length Duration, reason Encounter.reason*, diagnosis Encounter.diagnosis*, account Reference*, dietPreference CodeableConcept*, specialArrangement CodeableConcept*, specialCourtesy CodeableConcept*, admission Encounter.admission, location Encounter.location*
@R5 Encounter.participant: type CodeableConcept*, period Period, actor Reference
@R5 Encounter.reason: use CodeableConcept*, value CodeableReference*
@R5 Encounter.diagnosis: condition CodeableReference*, use CodeableConcept*
@R5 Encounter.admission: preAdmissionIdentifier Identifier, origin Reference, admitSource CodeableConcept, reAdmission CodeableConcept, destination Reference, dischargeDisposition CodeableConcept
@R5 Encounter.location: location Reference, status code, form CodeableConcept, period Period
@R5 Condition: identifier Identifier*, clinicalStatus CodeableConcept, verificationStatus CodeableConcept, category CodeableConcept*, severity CodeableConcept, code CodeableConcept, bodySite CodeableConcept*, subject Reference, encounter Reference, onset[x] dateTime|Age|Period|Range|string, abatement[x] dateTime|Age|Period|Range|string, recordedDate dateTime, participant Condition.participant*, stage Condition.stage*, evidence CodeableReference*, note Annotation*
@R5 Condition.participant: function CodeableConcept, actor Reference
@R5 MedicationRequest: identifier Identifier*, basedOn Reference*, priorPrescription Reference, groupIdentifier Identifier, status code, statusReason CodeableConcept, statusChanged dateTime, intent code, category CodeableConcept*, priority code, doNotPerform boolean, medication CodeableReference, subject Reference, informationSource Reference*, encounter Reference, supportingInformation Reference*, authoredOn dateTime, requester Reference, reported boolean, performerType CodeableConcept, performer Reference*, device CodeableReference*, recorder Reference, reason CodeableReference*, courseOfTherapyType CodeableConcept, insurance Reference*, note Annotation*, renderedDosageInstruction markdown, effectiveDosePeriod Period, dosageInstruction Dosage*, dispenseRequest MedicationRequest.dispenseRequest, substitution MedicationRequest.substitution, eventHistory Reference*
@R5 VirtualServiceDetail: channelType Coding, address[x] url|string|ContactPoint, additionalInfo url*, maxParticipants positiveInt, sessionKey string
"#;

/// FHIR primitive types.
pub const PRIMITIVES: &[&str] = &[
    "boolean", "integer", "integer64", "decimal", "positiveInt", "unsignedInt", "string", "uri", "url", "canonical", "code", "oid", "id", "uuid", "markdown", "base64Binary", "date",
    "dateTime", "instant", "time", "xhtml",
];

/// The resources that are not `DomainResource`s.
const PLAIN_RESOURCES: &[&str] = &["Bundle", "Parameters", "Binary"];

/// The resource types of the registry.
pub const RESOURCES: &[&str] = &[
    "Patient", "Practitioner", "Organization", "Observation", "Encounter", "Bundle", "OperationOutcome", "CapabilityStatement", "Parameters", "Condition", "Procedure", "Medication",
    "MedicationRequest", "AllergyIntolerance", "DiagnosticReport", "Immunization", "Location", "Device", "ServiceRequest", "Appointment", "Binary", "Basic", "StructureDefinition",
];

/// A FHIR release. The registry holds R4 definitions; DSTU3 and R5 override the resources
/// that changed shape between releases (Observation, Encounter, Condition,
/// AllergyIntolerance, MedicationRequest, Patient, Dosage...). R4B shares R4's definitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FhirVersion {
    Dstu3,
    #[default]
    R4,
    R4B,
    R5,
}

impl FhirVersion {
    /// Parse a `fhirVersion` option value (`DSTU3`, `R4`, `R4B`, `R5`).
    pub fn parse(s: &str) -> Option<FhirVersion> {
        match s.trim().to_ascii_uppercase().as_str() {
            "DSTU3" | "STU3" | "3.0" | "3.0.2" => Some(FhirVersion::Dstu3),
            "R4" | "4.0" | "4.0.1" => Some(FhirVersion::R4),
            "R4B" | "4.3" | "4.3.0" => Some(FhirVersion::R4B),
            "R5" | "5.0" | "5.0.0" => Some(FhirVersion::R5),
            _ => None,
        }
    }

    /// The `fhirVersion` string of a CapabilityStatement.
    pub fn number(self) -> &'static str {
        match self {
            FhirVersion::Dstu3 => "3.0.2",
            FhirVersion::R4 => "4.0.1",
            FhirVersion::R4B => "4.3.0",
            FhirVersion::R5 => "5.0.0",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            FhirVersion::Dstu3 => "DSTU3",
            FhirVersion::R4 | FhirVersion::R4B => "R4",
            FhirVersion::R5 => "R5",
        }
    }
}

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

type Registry = HashMap<String, Vec<Def>>;

fn registries() -> &'static HashMap<&'static str, Registry> {
    static R: OnceLock<HashMap<&'static str, Registry>> = OnceLock::new();
    R.get_or_init(|| {
        let mut all: HashMap<&'static str, Registry> = HashMap::new();
        for tag in ["DSTU3", "R4", "R5"] {
            all.insert(tag, HashMap::new());
        }
        let mut overrides: Vec<(&str, String, Vec<Def>)> = Vec::new();
        for line in DEFS.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let (tag, line) = match line.strip_prefix('@') {
                Some(rest) => {
                    let (t, l) = rest.split_once(' ').unwrap_or((rest, ""));
                    (Some(t), l)
                }
                None => (None, line),
            };
            let (owner, rest) = line.split_once(':').unwrap_or((line, ""));
            let defs: Vec<Def> = rest
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
            match tag {
                None => {
                    for r in all.values_mut() {
                        r.insert(owner.trim().to_string(), defs.clone());
                    }
                }
                Some(t) => overrides.push((if t == "DSTU3" { "DSTU3" } else { "R5" }, owner.trim().to_string(), defs)),
            }
        }
        for (tag, owner, defs) in overrides {
            all.get_mut(tag).unwrap().insert(owner, defs);
        }
        all
    })
}

fn registry(v: FhirVersion) -> &'static Registry {
    &registries()[v.tag()]
}

/// Whether a type is a known resource type.
pub fn is_resource(t: &str) -> bool {
    matches!(t, "Resource" | "DomainResource") || RESOURCES.contains(&t)
}

fn alias(t: &str) -> &str {
    match t {
        "Age" | "Duration" | "Distance" | "Count" | "SimpleQuantity" | "MoneyQuantity" => "Quantity",
        other => other,
    }
}

/// The type a choice suffix names (`Boolean` → `boolean`, `Quantity` → `Quantity`).
pub fn choice_type(suffix: &str) -> String {
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

fn lookup_in(v: FhirVersion, owner: &str, name: &str) -> Option<(String, bool, usize)> {
    let defs = registry(v).get(alias(owner))?;
    if let Some((i, d)) = defs.iter().enumerate().find(|(_, d)| d.name == name && d.choices.is_none()) {
        return Some((d.ty.clone(), d.many, i));
    }
    for (i, d) in defs.iter().enumerate().filter(|(_, d)| d.choices.is_some()) {
        if let Some(rest) = name.strip_prefix(&d.name) {
            if rest.chars().next().map(char::is_uppercase).unwrap_or(false) {
                let t = choice_type(rest);
                let allowed = d.choices.as_ref().map(|c| c.is_empty() || c.iter().any(|x| choice_type(x) == t || x == rest)).unwrap_or(true);
                if allowed {
                    return Some((t, d.many, i));
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
    element_v(FhirVersion::R4, owner, name, resource)
}

/// [`element`] for a given FHIR release.
pub fn element_v(v: FhirVersion, owner: &str, name: &str, resource: bool) -> Option<(String, bool)> {
    element_info(v, owner, name, resource).map(|(t, m, _)| (t, m))
}

/// Type, repetition and spec order key of an element.
fn element_info(v: FhirVersion, owner: &str, name: &str, resource: bool) -> Option<(String, bool, usize)> {
    let resource = resource || is_resource(owner);
    if resource {
        if let Some((t, m, i)) = lookup_in(v, "Resource", name) {
            return Some((t, m, i));
        }
        if !PLAIN_RESOURCES.contains(&owner) {
            if let Some((t, m, i)) = lookup_in(v, "DomainResource", name) {
                return Some((t, m, 100 + i));
            }
        }
        if let Some((t, m, i)) = lookup_in(v, owner, name) {
            return Some((t, m, 1000 + i));
        }
        return None;
    }
    match name {
        "extension" => return Some(("Extension".into(), true, 0)),
        "modifierExtension" if owner.contains('.') => return Some(("Extension".into(), true, 1)),
        _ => {}
    }
    if let Some((t, m, i)) = lookup_in(v, owner, name) {
        return Some((t, m, 1000 + i));
    }
    if name == "modifierExtension" {
        return Some(("Extension".into(), true, 1));
    }
    None
}

/// The spec order key of element `name` in `owner` (`None` for unknown elements, which are
/// written after the known ones). A leading `_` (primitive extensions) is ignored.
pub fn order_key(v: FhirVersion, owner: &str, name: &str, resource: bool) -> Option<usize> {
    element_info(v, owner, name.trim_start_matches('_'), resource).map(|(_, _, i)| i)
}

/// The element names of a type in spec order (`name[x]` for choices), for profile tooling.
pub fn element_names(v: FhirVersion, owner: &str) -> Vec<(String, bool)> {
    registry(v).get(alias(owner)).map(|d| d.iter().map(|d| (if d.choices.is_some() { format!("{}[x]", d.name) } else { d.name.clone() }, d.many)).collect()).unwrap_or_default()
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
        assert_eq!(element("MedicationRequest", "dosageInstruction", true), Some(("Dosage".into(), true)));
    }

    #[test]
    fn versions_and_order() {
        assert_eq!(element_v(FhirVersion::Dstu3, "Condition", "clinicalStatus", true), Some(("code".into(), false)));
        assert_eq!(element_v(FhirVersion::R4, "Condition", "clinicalStatus", true), Some(("CodeableConcept".into(), false)));
        assert_eq!(element_v(FhirVersion::R5, "Encounter", "class", true), Some(("CodeableConcept".into(), true)));
        assert_eq!(element_v(FhirVersion::Dstu3, "Observation", "context", true), Some(("Reference".into(), false)));
        assert_eq!(element_v(FhirVersion::R4, "Observation", "context", true), None);
        assert_eq!(element_v(FhirVersion::R5, "MedicationRequest", "medication", true), Some(("CodeableReference".into(), false)));
        assert_eq!(FhirVersion::parse("r4b"), Some(FhirVersion::R4B));
        let k = |n: &str| order_key(FhirVersion::R4, "Patient", n, true).unwrap();
        assert!(k("id") < k("meta") && k("meta") < k("text") && k("text") < k("extension") && k("extension") < k("identifier") && k("identifier") < k("name") && k("name") < k("birthDate"));
        let h = |n: &str| order_key(FhirVersion::R4, "HumanName", n, false).unwrap();
        assert!(h("extension") < h("use") && h("family") < h("given"));
    }
}
