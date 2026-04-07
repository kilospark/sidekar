import SwiftUI
import UIKit

struct KeyboardPaddingModifier: ViewModifier {
    let keyboardHeight: CGFloat
    func body(content: Content) -> some View {
        content
            .padding(.bottom, keyboardHeight)
    }
}

struct TerminalContainerView: View {
    let session: Session
    @EnvironmentObject var authService: AuthService
    @StateObject private var wsManager: WebSocketManager
    @Environment(\.scenePhase) private var scenePhase
    @State private var keyboardHeight: CGFloat = 0
    @FocusState private var isInputFocused: Bool

    init(session: Session) {
        self.session = session
        _wsManager = StateObject(wrappedValue: WebSocketManager(
            sessionId: session.id,
            relayURL: session.relay_url ?? "https://relay.sidekar.dev"
        ))
    }

    var body: some View {
        VStack(spacing: 0) {
            statusBar
            TerminalViewWrapper(wsManager: wsManager, isFocused: $isInputFocused)
                .modifier(KeyboardPaddingModifier(keyboardHeight: keyboardHeight))
        }
        .navigationBarTitleDisplayMode(.inline)
        .navigationTitle(session.name)
        .toolbarBackground(.visible, for: .navigationBar)
        .toolbarColorScheme(.dark, for: .navigationBar)
        .preferredColorScheme(.dark)
        .onAppear {
            if let jwt = authService.validJWT() {
                wsManager.connect(jwt: jwt)
            }
            setupKeyboardObservers()
        }
        .onDisappear {
            wsManager.disconnect()
            NotificationCenter.default.removeObserver(self)
        }
        .onChange(of: scenePhase) { _, newPhase in
            if newPhase == .active, let jwt = authService.validJWT() {
                wsManager.reconnectIfNeeded(jwt: jwt)
            }
        }
    }

    private func setupKeyboardObservers() {
        NotificationCenter.default.addObserver(
            forName: UIResponder.keyboardWillShowNotification,
            object: nil,
            queue: .main
        ) { notification in
            if let keyboardFrame = notification.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? CGRect {
                keyboardHeight = keyboardFrame.height
            }
        }
        NotificationCenter.default.addObserver(
            forName: UIResponder.keyboardWillHideNotification,
            object: nil,
            queue: .main
        ) { _ in
            keyboardHeight = 0
        }
    }

    private var statusBar: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(statusColor)
                .frame(width: 6, height: 6)
            Text(statusText)
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(.secondary)
            Spacer()
            Text("\(wsManager.terminalCols)x\(wsManager.terminalRows)")
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 12)
        .frame(height: 28)
        .background(Color(white: 0.08))
    }

    private var statusColor: Color {
        switch wsManager.connectionState {
        case .connected: return .green
        case .connecting: return .yellow
        case .disconnected, .error: return .red
        }
    }

    private var statusText: String {
        switch wsManager.connectionState {
        case .connected: return "connected"
        case .connecting: return "connecting..."
        case .disconnected: return "disconnected"
        case .error(let msg): return msg
        }
    }
}
