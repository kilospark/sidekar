import Foundation

enum APIError: Error, LocalizedError {
    case unauthorized
    case serverError(Int)
    case decodingError
    case networkError(Error)

    var errorDescription: String? {
        switch self {
        case .unauthorized: return "Session expired"
        case .serverError(let code): return "Server error (\(code))"
        case .decodingError: return "Invalid response"
        case .networkError(let err): return err.localizedDescription
        }
    }
}

struct SessionsResponse: Codable {
    let sessions: [Session]
}

enum APIClient {
    private static let baseURL = "https://sidekar.dev"

    static func fetchSessions(jwt: String) async throws -> [Session] {
        var request = URLRequest(url: URL(string: "\(baseURL)/api/sessions")!)
        request.setValue("Bearer \(jwt)", forHTTPHeaderField: "Authorization")

        let (data, response) = try await URLSession.shared.data(for: request)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0

        if status == 401 { throw APIError.unauthorized }
        if status >= 400 { throw APIError.serverError(status) }

        guard let result = try? JSONDecoder().decode(SessionsResponse.self, from: data) else {
            throw APIError.decodingError
        }
        return result.sessions
    }
}
