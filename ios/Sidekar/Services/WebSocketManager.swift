import Foundation

enum ConnectionState: Equatable {
    case disconnected
    case connecting
    case connected
    case error(String)
}

protocol WebSocketDataDelegate: AnyObject {
    func didReceiveTerminalData(_ data: Data)
}

@MainActor
class WebSocketManager: ObservableObject {
    @Published var connectionState: ConnectionState = .disconnected
    @Published var terminalCols: Int = 80
    @Published var terminalRows: Int = 24

    let sessionId: String
    let relayURL: String
    weak var dataDelegate: WebSocketDataDelegate?

    private var webSocketTask: URLSessionWebSocketTask?
    private var reconnectAttempt = 0
    private var reconnectTask: Task<Void, Never>?
    private var expectScrollbackBytes = 0
    private var isShutdown = false

    private var dataBuffer: [Data] = []
    private var drainTimer: Timer?
    private let bufferLock = NSLock()
    private let drainInterval: TimeInterval = 0.02

    init(sessionId: String, relayURL: String) {
        self.sessionId = sessionId
        self.relayURL = relayURL
    }

    deinit {
        drainTimer?.invalidate()
    }

    func connect(jwt: String) {
        guard !isShutdown else { return }
        connectionState = .connecting

        let wsOrigin = relayURL
            .replacingOccurrences(of: "https://", with: "wss://")
            .replacingOccurrences(of: "http://", with: "ws://")
        let urlString = "\(wsOrigin)/session/\(sessionId)?token=\(jwt)"
        guard let url = URL(string: urlString) else {
            connectionState = .error("Invalid relay URL")
            return
        }

        let task = URLSession.shared.webSocketTask(with: url)
        webSocketTask = task
        task.resume()
        connectionState = .connected
        reconnectAttempt = 0
        receiveLoop(task: task, jwt: jwt)
    }

    func sendInput(_ data: Data) {
        webSocketTask?.send(.data(data)) { _ in }
    }

    func disconnect() {
        isShutdown = true
        reconnectTask?.cancel()
        reconnectTask = nil
        webSocketTask?.cancel(with: .goingAway, reason: nil)
        webSocketTask = nil
        connectionState = .disconnected
        drainTimer?.invalidate()
        drainTimer = nil
        bufferLock.lock()
        dataBuffer.removeAll()
        bufferLock.unlock()
    }

    func reconnectIfNeeded(jwt: String) {
        if case .connected = connectionState { return }
        connect(jwt: jwt)
    }

    // MARK: - Private

    private func receiveLoop(task: URLSessionWebSocketTask, jwt: String) {
        task.receive { [weak self] result in
            Task { @MainActor in
                guard let self, self.webSocketTask === task else { return }
                switch result {
                case .success(.string(let text)):
                    self.handleTextMessage(text)
                    self.receiveLoop(task: task, jwt: jwt)
                case .success(.data(let data)):
                    self.handleBinaryMessage(data)
                    self.receiveLoop(task: task, jwt: jwt)
                case .failure(let error):
                    self.handleDisconnect(error: error, jwt: jwt)
                @unknown default:
                    self.receiveLoop(task: task, jwt: jwt)
                }
            }
        }
    }

    private func handleTextMessage(_ text: String) {
        guard let data = text.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return
        }

        let type = json["type"] as? String

        if type == "session", (json["v"] as? Int) == 1 {
            expectScrollbackBytes = json["scrollback_bytes"] as? Int ?? 0
            if let cols = json["cols"] as? Int { terminalCols = cols }
            if let rows = json["rows"] as? Int { terminalRows = rows }
            return
        }

        if type == "pty", (json["event"] as? String) == "resize" {
            if let cols = json["cols"] as? Int { terminalCols = cols }
            if let rows = json["rows"] as? Int { terminalRows = rows }
            return
        }
    }

    private func handleBinaryMessage(_ data: Data) {
        if expectScrollbackBytes > 0 {
            expectScrollbackBytes = 0
        }
        bufferData(data)
    }

    private func bufferData(_ data: Data) {
        bufferLock.lock()
        dataBuffer.append(data)
        bufferLock.unlock()

        if drainTimer == nil {
            startDrainTimer()
        }
    }

    private func startDrainTimer() {
        drainTimer = Timer(timeInterval: drainInterval, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.drainBuffer()
            }
        }
        RunLoop.main.add(drainTimer!, forMode: .common)
    }

    private func drainBuffer() {
        bufferLock.lock()
        let buffer = dataBuffer
        dataBuffer.removeAll()
        bufferLock.unlock()

        for data in buffer {
            dataDelegate?.didReceiveTerminalData(data)
        }

        if buffer.isEmpty && drainTimer != nil {
            drainTimer?.invalidate()
            drainTimer = nil
        }
    }

    private func handleDisconnect(error: Error, jwt: String) {
        webSocketTask = nil
        connectionState = .error("Disconnected")
        scheduleReconnect(jwt: jwt)
    }

    private func scheduleReconnect(jwt: String) {
        guard !isShutdown else { return }
        reconnectTask?.cancel()
        let delay = min(30.0, pow(2.0, Double(reconnectAttempt)))
        reconnectAttempt += 1

        reconnectTask = Task {
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            guard !Task.isCancelled, !isShutdown else { return }
            connect(jwt: jwt)
        }
    }
}
