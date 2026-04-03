import Foundation

struct Session: Codable, Identifiable {
    let id: String
    let name: String
    let agent_type: String
    let cwd: String
    let hostname: String
    let nickname: String?
    let connected_at: String
    let relay_url: String?

    var relativeTime: String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        guard let date = formatter.date(from: connected_at)
            ?? ISO8601DateFormatter().date(from: connected_at) else {
            return ""
        }
        let seconds = Int(Date().timeIntervalSince(date))
        if seconds < 60 { return "\(seconds)s ago" }
        if seconds < 3600 { return "\(seconds / 60)m ago" }
        if seconds < 86400 { return "\(seconds / 3600)h ago" }
        return "\(seconds / 86400)d ago"
    }
}
