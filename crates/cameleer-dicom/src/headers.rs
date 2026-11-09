//! Message headers set and read by the DICOM components and data formats
//! (`Camel<Component><Name>`, as Camel names them).

/// AE title of the calling (requesting) application.
pub const CALLING_AET: &str = "CamelDicomCallingAet";
/// AE title of the called application.
pub const CALLED_AET: &str = "CamelDicomCalledAet";
/// SOP Class UID of the object or of the request.
pub const SOP_CLASS_UID: &str = "CamelDicomSopClassUid";
/// SOP Instance UID of the object.
pub const SOP_INSTANCE_UID: &str = "CamelDicomSopInstanceUid";
/// Transfer syntax UID of the object.
pub const TRANSFER_SYNTAX_UID: &str = "CamelDicomTransferSyntaxUid";
/// DIMSE status of the last response (an integer, `0` for success).
pub const STATUS: &str = "CamelDicomStatus";
/// Error Comment (0000,0902) of a failure or warning response.
pub const ERROR_COMMENT: &str = "CamelDicomErrorComment";
/// Address of the peer (`host:port`).
pub const REMOTE_ADDRESS: &str = "CamelDicomRemoteAddress";
/// The DIMSE command (`C-ECHO`, `C-STORE`, `C-FIND`, `C-MOVE`, `C-GET`).
pub const COMMAND: &str = "CamelDicomCommand";
/// The operation of a `dicom:` producer for this message (overrides `operation`).
pub const OPERATION: &str = "CamelDicomOperation";
/// Destination AE title of a C-MOVE.
pub const MOVE_DESTINATION: &str = "CamelDicomMoveDestination";
/// Query/Retrieve level (`PATIENT`, `STUDY`, `SERIES`, `IMAGE`).
pub const QUERY_LEVEL: &str = "CamelDicomQueryLevel";
/// Message ID of the request.
pub const MESSAGE_ID: &str = "CamelDicomMessageId";
/// Priority (`LOW`, `MEDIUM`, `HIGH`).
pub const PRIORITY: &str = "CamelDicomPriority";
/// Sub-operation counts of a C-MOVE or C-GET.
pub const COMPLETED: &str = "CamelDicomCompleted";
pub const FAILED: &str = "CamelDicomFailed";
pub const WARNING: &str = "CamelDicomWarning";
/// AE title of the originator of a C-MOVE that caused this C-STORE.
pub const MOVE_ORIGINATOR_AET: &str = "CamelDicomMoveOriginatorAet";
/// Number of results of a C-FIND or QIDO-RS query.
pub const RESULT_COUNT: &str = "CamelDicomResultCount";
/// The DICOMweb operation (`qido`, `wado`, `stow`, `metadata`).
pub const DICOMWEB_OPERATION: &str = "CamelDicomWebOperation";
/// Study, series and instance UIDs of a DICOMweb request.
pub const STUDY_INSTANCE_UID: &str = "CamelDicomStudyInstanceUid";
pub const SERIES_INSTANCE_UID: &str = "CamelDicomSeriesInstanceUid";
/// Query parameters of a QIDO-RS request (a map).
pub const QUERY: &str = "CamelDicomQuery";
/// HTTP status of a DICOMweb response.
pub const HTTP_STATUS: &str = "CamelDicomHttpStatus";
