import Foundation
import Darwin

enum TransportEvent: Sendable {
    case line(Data)
    case stderr(String)
    case failure(String)
    case ended(Int32)
}

@MainActor
final class Sidecar {
    private var process: Process?
    private var input: FileHandle?
    private var continuation: AsyncStream<TransportEvent>.Continuation?
    private let writes = DispatchQueue(label: "dev.sprowt.desktop.stdin")

    func start(project: URL, dataDirectory: String?, executable: URL? = nil) throws -> AsyncStream<TransportEvent> {
        try start(arguments: ["desktop", "--project", project.path], directory: project, dataDirectory: dataDirectory, executable: executable)
    }

    func catalog(dataDirectory: String?, removing project: Int64? = nil, executable: URL? = nil) throws -> AsyncStream<TransportEvent> {
        let arguments = ["projects"] + (project.map { ["--remove", String($0)] } ?? [])
        return try start(arguments: arguments, directory: nil, dataDirectory: dataDirectory, executable: executable)
    }

    private func start(arguments: [String], directory: URL?, dataDirectory: String?, executable: URL?) throws -> AsyncStream<TransportEvent> {
        let binary = executable ?? Bundle.main.executableURL!
            .deletingLastPathComponent().appendingPathComponent("sprowt-harness")
        let process = Process()
        process.executableURL = binary
        process.arguments = arguments
        if let dataDirectory { process.arguments! += ["--data-dir", dataDirectory] }
        process.currentDirectoryURL = directory
        var environment = ProcessInfo.processInfo.environment
        environment["PATH"] = Self.searchPath(environment: environment)
        process.environment = environment
        let stdin = Pipe(), stdout = Pipe(), stderr = Pipe()
        process.standardInput = stdin
        process.standardOutput = stdout
        process.standardError = stderr
        guard fcntl(stdin.fileHandleForWriting.fileDescriptor, F_SETNOSIGPIPE, 1) != -1 else {
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
        let (stream, continuation) = AsyncStream.makeStream(of: TransportEvent.self)
        try process.run()
        try? stdin.fileHandleForReading.close()
        try? stdout.fileHandleForWriting.close()
        try? stderr.fileHandleForWriting.close()
        self.process = process
        self.input = stdin.fileHandleForWriting
        self.continuation = continuation
        let stderrFinished = DispatchGroup()
        stderrFinished.enter()
        DispatchQueue.global(qos: .utility).async {
            defer { stderrFinished.leave() }
            while true {
                let data = stderr.fileHandleForReading.availableData
                if data.isEmpty { break }
                continuation.yield(.stderr(String(decoding: data, as: UTF8.self)))
            }
            try? stderr.fileHandleForReading.close()
        }
        DispatchQueue.global(qos: .userInitiated).async {
            var buffer = Data()
            while true {
                let data = stdout.fileHandleForReading.availableData
                if data.isEmpty { break }
                buffer.append(data)
                while let newline = buffer.firstIndex(of: 0x0a) {
                    let line = Data(buffer[..<newline])
                    buffer.removeSubrange(...newline)
                    if !line.isEmpty { continuation.yield(.line(line)) }
                }
            }
            if !buffer.isEmpty { continuation.yield(.failure(ProtocolError.incompleteLine.localizedDescription)) }
            try? stdout.fileHandleForReading.close()
            process.waitUntilExit()
            stderrFinished.wait()
            continuation.yield(.ended(process.terminationStatus))
            continuation.finish()
        }
        return stream
    }

    func send(_ request: Request) throws {
        guard let input, let continuation else { throw CocoaError(.fileWriteUnknown) }
        let data = try request.encodedLine()
        writes.async {
            do { try input.write(contentsOf: data) }
            catch { continuation.yield(.failure("Could not send to the Rust engine: \(error.localizedDescription)")) }
        }
    }

    func closeInput() {
        guard let input else { return }
        self.input = nil
        writes.async { try? input.close() }
    }

    static func searchPath(environment: [String: String]) -> String {
        let paths = (environment["PATH"] ?? "/usr/bin:/bin:/usr/sbin:/sbin").split(separator: ":").map(String.init)
        let userHome = environment["HOME"] ?? NSHomeDirectory()
        let additions = ["/opt/homebrew/bin", "/usr/local/bin", "\(userHome)/.cargo/bin", "\(userHome)/.local/bin"]
        return (paths + additions.filter { !paths.contains($0) }).joined(separator: ":")
    }
}
