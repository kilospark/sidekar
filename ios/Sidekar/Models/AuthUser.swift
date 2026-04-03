import Foundation

struct AuthUser: Codable {
    let sub: String
    let login: String
    let name: String
    let exp: Int?
}

func decodeJWTPayload(_ jwt: String) -> AuthUser? {
    let parts = jwt.split(separator: ".")
    guard parts.count == 3 else { return nil }
    var base64 = String(parts[1])
        .replacingOccurrences(of: "-", with: "+")
        .replacingOccurrences(of: "_", with: "/")
    while base64.count % 4 != 0 { base64.append("=") }
    guard let data = Data(base64Encoded: base64) else { return nil }
    return try? JSONDecoder().decode(AuthUser.self, from: data)
}

func isJWTExpired(_ jwt: String) -> Bool {
    guard let user = decodeJWTPayload(jwt), let exp = user.exp else { return true }
    return Date(timeIntervalSince1970: TimeInterval(exp)) < Date()
}
