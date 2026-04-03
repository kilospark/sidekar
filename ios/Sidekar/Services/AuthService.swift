import AuthenticationServices
import Foundation
import SwiftUI

struct AuthProvider: Codable, Identifiable {
    let id: String
    let name: String
    let url: String
}

struct ProvidersResponse: Codable {
    let providers: [AuthProvider]
}

@MainActor
class AuthService: NSObject, ObservableObject {
    @Published var jwt: String?
    @Published var user: AuthUser?
    @Published var providers: [AuthProvider] = []
    @Published var providersLoaded = false

    private static let keychainKey = "sidekar_jwt"
    private static let baseURL = "https://sidekar.dev"

    var isAuthenticated: Bool { jwt != nil }

    override init() {
        super.init()
        if let stored = KeychainHelper.readString(Self.keychainKey) {
            if isJWTExpired(stored) {
                KeychainHelper.delete(Self.keychainKey)
            } else {
                jwt = stored
                user = decodeJWTPayload(stored)
            }
        }
    }

    func fetchProviders() async {
        guard let url = URL(string: "\(Self.baseURL)/api/auth/session?providers") else { return }
        do {
            let (data, _) = try await URLSession.shared.data(from: url)
            let result = try JSONDecoder().decode(ProvidersResponse.self, from: data)
            providers = result.providers
        } catch {}
        providersLoaded = true
    }

    func login(provider: AuthProvider) {
        guard let url = URL(string: "\(Self.baseURL)\(provider.url)?redirect=mobile") else { return }
        let session = ASWebAuthenticationSession(
            url: url,
            callbackURLScheme: "sidekar"
        ) { [weak self] callbackURL, error in
            guard let callbackURL, error == nil else { return }
            Task { @MainActor in
                self?.handleCallback(url: callbackURL)
            }
        }
        session.presentationContextProvider = self
        session.prefersEphemeralWebBrowserSession = false
        session.start()
    }

    func handleCallback(url: URL) {
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let token = components.queryItems?.first(where: { $0.name == "token" })?.value else {
            return
        }
        guard !isJWTExpired(token) else { return }
        _ = KeychainHelper.saveString(Self.keychainKey, value: token)
        jwt = token
        user = decodeJWTPayload(token)
    }

    func logout() {
        KeychainHelper.delete(Self.keychainKey)
        jwt = nil
        user = nil
    }

    func validJWT() -> String? {
        guard let jwt, !isJWTExpired(jwt) else {
            logout()
            return nil
        }
        return jwt
    }
}

extension AuthService: ASWebAuthenticationPresentationContextProviding {
    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        guard let scene = UIApplication.shared.connectedScenes.first as? UIWindowScene,
              let window = scene.windows.first else {
            return ASPresentationAnchor()
        }
        return window
    }
}
