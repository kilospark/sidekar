import SwiftUI

@main
struct SidekarApp: App {
    @StateObject private var authService = AuthService()

    var body: some Scene {
        WindowGroup {
            Group {
                if authService.isAuthenticated {
                    SessionsListView()
                } else {
                    LoginView()
                }
            }
            .environmentObject(authService)
            .onOpenURL { url in
                authService.handleCallback(url: url)
            }
        }
    }
}
