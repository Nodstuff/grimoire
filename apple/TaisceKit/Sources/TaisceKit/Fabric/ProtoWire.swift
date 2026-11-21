import Foundation

// The protobuf wire format, the small part of it the signed Agent Fabric
// messages use (`proto/taisce/runtime/v1/runtime.proto`, SIGNING): varints
// and length-delimited fields. Hand-written rather than SwiftProtobuf:
// the app signs four messages and reads one, the encoding must be the
// canonical one byte for byte (checked against `signing-vectors.json`),
// and a generator would add a build step and a package for that.
//
// Canonical, as the .proto states it: fields in ascending number, values
// equal to the proto3 default left out, a set oneof member or sub-message
// written even when empty, repeated fields one entry each, nothing unknown.
// The writer leaves ordering to its caller (each message writes its fields
// in number order) and enforces the rest.

enum WireType: UInt8 {
    case varint = 0
    case fixed64 = 1
    case lengthDelimited = 2
    case fixed32 = 5
}

struct ProtoWriter {
    private(set) var data = Data()

    mutating func varint(_ value: UInt64) {
        var v = value
        while v >= 0x80 {
            data.append(UInt8(v & 0x7f) | 0x80)
            v >>= 7
        }
        data.append(UInt8(v))
    }

    mutating func tag(_ field: Int, _ wire: WireType) {
        varint(UInt64(field) << 3 | UInt64(wire.rawValue))
    }

    /// uint32, uint64 and enums: left out at 0.
    mutating func uint(_ field: Int, _ value: UInt64) {
        guard value != 0 else { return }
        tag(field, .varint)
        varint(value)
    }

    /// int64: left out at 0; a negative value is ten bytes, two's complement.
    mutating func int(_ field: Int, _ value: Int64) {
        guard value != 0 else { return }
        tag(field, .varint)
        varint(UInt64(bitPattern: value))
    }

    mutating func bool(_ field: Int, _ value: Bool) {
        guard value else { return }
        tag(field, .varint)
        varint(1)
    }

    /// Left out when empty.
    mutating func string(_ field: Int, _ value: String) {
        bytes(field, Data(value.utf8))
    }

    /// Left out when empty.
    mutating func bytes(_ field: Int, _ value: Data) {
        guard !value.isEmpty else { return }
        lengthDelimited(field, value)
    }

    /// A set sub-message or oneof member: written even when empty.
    mutating func message(_ field: Int, _ encoded: Data) {
        lengthDelimited(field, encoded)
    }

    private mutating func lengthDelimited(_ field: Int, _ value: Data) {
        tag(field, .lengthDelimited)
        varint(UInt64(value.count))
        data.append(value)
    }
}

/// Why bytes could not be read as the message they claim to be.
public enum ProtoError: Error, Equatable, Sendable {
    case truncated
    case malformedVarint
    /// a field this build has no number for: the signer meant something
    /// this verifier cannot honour (SIGNING), so the whole message is refused
    case unknownField(Int)
    /// a known field with the wrong wire type
    case wrongWireType(field: Int)
    case invalidUTF8(field: Int)
}

/// Reads fields one at a time. Strict: a message type reads the fields it
/// knows and refuses any other (`ProtoError.unknownField`).
struct ProtoReader {
    private let data: Data
    private var offset: Data.Index

    init(_ data: Data) {
        self.data = data
        offset = data.startIndex
    }

    var isAtEnd: Bool { offset >= data.endIndex }

    mutating func varint() throws -> UInt64 {
        var result: UInt64 = 0
        var shift: UInt64 = 0
        while true {
            guard offset < data.endIndex else { throw ProtoError.truncated }
            let byte = data[offset]
            offset += 1
            guard shift < 64 else { throw ProtoError.malformedVarint }
            result |= UInt64(byte & 0x7f) << shift
            if byte & 0x80 == 0 { return result }
            shift += 7
        }
    }

    /// The next field's number and wire type.
    mutating func next() throws -> (field: Int, wire: UInt8) {
        let key = try varint()
        return (Int(key >> 3), UInt8(key & 7))
    }

    mutating func lengthDelimited(field: Int, wire: UInt8) throws -> Data {
        guard wire == WireType.lengthDelimited.rawValue else { throw ProtoError.wrongWireType(field: field) }
        let length = try varint()
        guard length <= UInt64(data.endIndex - offset) else { throw ProtoError.truncated }
        let end = offset + Int(length)
        defer { offset = end }
        return Data(data[offset..<end])
    }

    mutating func string(field: Int, wire: UInt8) throws -> String {
        let bytes = try lengthDelimited(field: field, wire: wire)
        guard let s = String(data: bytes, encoding: .utf8) else { throw ProtoError.invalidUTF8(field: field) }
        return s
    }

    mutating func uint(field: Int, wire: UInt8) throws -> UInt64 {
        guard wire == WireType.varint.rawValue else { throw ProtoError.wrongWireType(field: field) }
        return try varint()
    }

    mutating func int(field: Int, wire: UInt8) throws -> Int64 {
        Int64(bitPattern: try uint(field: field, wire: wire))
    }

    mutating func bool(field: Int, wire: UInt8) throws -> Bool {
        try uint(field: field, wire: wire) != 0
    }
}
