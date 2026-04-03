import SwiftTerm
import SwiftUI

struct TerminalViewWrapper: UIViewRepresentable {
    @ObservedObject var wsManager: WebSocketManager

    func makeUIView(context: Context) -> TerminalView {
        let tv = TerminalView(frame: .zero)
        tv.terminalDelegate = context.coordinator
        tv.nativeBackgroundColor = UIColor(red: 0.035, green: 0.035, blue: 0.043, alpha: 1)
        tv.nativeForegroundColor = UIColor(red: 0.98, green: 0.98, blue: 0.98, alpha: 1)
        tv.font = UIFont.monospacedSystemFont(ofSize: 13, weight: .regular)

        context.coordinator.terminalView = tv
        wsManager.dataDelegate = context.coordinator

        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            tv.becomeFirstResponder()
        }

        return tv
    }

    func updateUIView(_ tv: TerminalView, context: Context) {
        let cols = wsManager.terminalCols
        let rows = wsManager.terminalRows
        let terminal = tv.getTerminal()
        if terminal.cols != cols || terminal.rows != rows {
            tv.resize(cols: cols, rows: rows)
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(wsManager: wsManager)
    }

    class Coordinator: NSObject, TerminalViewDelegate, WebSocketDataDelegate {
        weak var terminalView: TerminalView?
        let wsManager: WebSocketManager

        init(wsManager: WebSocketManager) {
            self.wsManager = wsManager
        }

        // MARK: - TerminalViewDelegate

        func send(source: TerminalView, data: ArraySlice<UInt8>) {
            let payload = Data(data)
            Task { @MainActor in
                wsManager.sendInput(payload)
            }
        }

        func scrolled(source: TerminalView, position: Double) {}
        func setTerminalTitle(source: TerminalView, title: String) {}
        func sizeChanged(source: TerminalView, newCols: Int, newRows: Int) {}
        func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {}
        func rangeChanged(source: TerminalView, startY: Int, endY: Int) {}

        func requestOpenLink(source: TerminalView, link: String, params: [String: String]) {
            guard let url = URL(string: link) else { return }
            UIApplication.shared.open(url)
        }

        func clipboardCopy(source: TerminalView, content: Data) {
            if let text = String(data: content, encoding: .utf8) {
                UIPasteboard.general.string = text
            }
        }

        // MARK: - WebSocketDataDelegate

        func didReceiveTerminalData(_ data: Data) {
            DispatchQueue.main.async { [weak self] in
                self?.terminalView?.feed(byteArray: ArraySlice(data))
            }
        }
    }
}
