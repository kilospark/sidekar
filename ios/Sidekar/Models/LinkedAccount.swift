import Foundation

struct LinkedAccount: Codable, Identifiable {
    let id: String
    let name: String
    let login: String?
    let email: String?
}

struct AvailableProvider: Codable, Identifiable {
    let id: String
    let name: String
    let url: String
}

struct LinkedAccountsResponse: Codable {
    let linked: [LinkedAccount]
    let available: [AvailableProvider]
    let email: String?
}
