import AppKit
import CoreServices
import Darwin
import Foundation

enum LocalIPC {
    static let bundleID = "dev.openharness.computeruse"
    static let displayName = "Open Harness Computer Use"

    // The host creates a private directory before asking LaunchServices to start
    // this app. It passes a path, never the authentication secret, as an argument.
    static func connectIfRequested() throws -> Bool {
        let args = CommandLine.arguments
        guard args.count > 1 else { return false }
        guard args.count == 3, args[1] == "--socket-dir", Bundle.main.bundleIdentifier == bundleID else {
            throw ComputerError("INVALID_LAUNCH", "Launch the Computer Use app through its host bridge.")
        }
        let directory = args[2]
        guard directory.hasPrefix("/"), !directory.contains("\0") else { throw ComputerError("INVALID_IPC", "Invalid local channel directory.") }
        var info = stat()
        guard lstat(directory, &info) == 0, info.st_mode & mode_t(S_IFMT) == mode_t(S_IFDIR),
              info.st_uid == getuid(), info.st_mode & 0o777 == 0o700 else {
            throw ComputerError("INVALID_IPC", "Local channel directory must be private and owned by the current user.")
        }
        let tokenPath = directory + "/token"
        let tokenFD = Darwin.open(tokenPath, O_RDONLY | O_NOFOLLOW)
        guard tokenFD >= 0 else { throw ComputerError("INVALID_IPC", "Local channel token is unavailable.") }
        var tokenInfo = stat()
        guard fstat(tokenFD, &tokenInfo) == 0, tokenInfo.st_uid == getuid(), tokenInfo.st_mode & mode_t(S_IFMT) == mode_t(S_IFREG), tokenInfo.st_mode & 0o777 == 0o600, tokenInfo.st_size == 64 else {
            Darwin.close(tokenFD)
            throw ComputerError("INVALID_IPC", "Local channel token is not private.")
        }
        let tokenData = FileHandle(fileDescriptor: tokenFD, closeOnDealloc: true).readDataToEndOfFile()
        guard let token = String(data: tokenData, encoding: .utf8), token.count == 64, token.allSatisfy({ $0.isHexDigit }) else {
            throw ComputerError("INVALID_IPC", "Invalid local channel token.")
        }
        let socketPath = directory + "/control.sock"
        guard lstat(socketPath, &info) == 0, info.st_mode & mode_t(S_IFMT) == mode_t(S_IFSOCK), info.st_uid == getuid(), info.st_mode & 0o777 == 0o600 else {
            throw ComputerError("INVALID_IPC", "Local channel socket is not private.")
        }
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = Array(socketPath.utf8) + [0]
        guard pathBytes.count <= MemoryLayout.size(ofValue: address.sun_path) else { throw ComputerError("INVALID_IPC", "Local channel path is too long.") }
        withUnsafeMutableBytes(of: &address.sun_path) { raw in raw.copyBytes(from: pathBytes) }
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw ComputerError("INVALID_IPC", "Cannot create the local channel.") }
        defer { Darwin.close(fd) }
        let connected = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        var uid: uid_t = 0; var gid: gid_t = 0
        guard connected == 0, getpeereid(fd, &uid, &gid) == 0, uid == getuid() else { throw ComputerError("INVALID_IPC", "Local channel peer is not the current user.") }
        // Nested/staging apps may launch by URL before LS can resolve their ID.
        // Register this bundle through the public API so permission UI can show
        // its actual name and path. This does not request or change TCC grants.
        guard LSRegisterURL(Bundle.main.bundleURL as CFURL, true) == noErr else { throw ComputerError("APP_REGISTRATION_FAILED", "Cannot register the Computer Use app identity.") }
        guard dup2(fd, STDIN_FILENO) >= 0, dup2(fd, STDOUT_FILENO) >= 0 else { throw ComputerError("INVALID_IPC", "Cannot attach the local channel.") }
        ComputerHelper.emit(["protocol": 1, "token": token, "bundle_id": bundleID, "pid": getpid()])
        return true
    }
}
