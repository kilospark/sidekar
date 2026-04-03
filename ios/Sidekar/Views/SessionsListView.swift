import SwiftUI

struct SessionsListView: View {
    @EnvironmentObject var authService: AuthService
    @State private var sessions: [Session] = []
    @State private var isLoading = true
    @State private var errorMessage: String?

    private let refreshTimer = Timer.publish(every: 30, on: .main, in: .common).autoconnect()

    var body: some View {
        NavigationStack {
            Group {
                if isLoading && sessions.isEmpty {
                    ProgressView("Loading sessions...")
                } else if sessions.isEmpty {
                    ContentUnavailableView(
                        "No Active Sessions",
                        systemImage: "terminal",
                        description: Text("Start an agent with sidekar to see it here.")
                    )
                } else {
                    sessionsList
                }
            }
            .navigationTitle("Sessions")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        if let user = authService.user {
                            Text(user.login)
                        }
                        Button("Log Out", role: .destructive) {
                            authService.logout()
                        }
                    } label: {
                        Image(systemName: "person.circle")
                    }
                }
            }
            .refreshable { await loadSessions() }
            .task { await loadSessions() }
            .onReceive(refreshTimer) { _ in
                Task { await loadSessions() }
            }
        }
        .preferredColorScheme(.dark)
    }

    private var sessionsList: some View {
        List(sessions) { session in
            NavigationLink(destination: TerminalContainerView(session: session)) {
                SessionCardView(session: session)
            }
        }
        .listStyle(.plain)
    }

    private func loadSessions() async {
        guard let jwt = authService.validJWT() else { return }
        do {
            let fetched = try await APIClient.fetchSessions(jwt: jwt)
            sessions = fetched
            errorMessage = nil
        } catch {
            if case APIError.unauthorized = error {
                authService.logout()
            } else {
                errorMessage = error.localizedDescription
            }
        }
        isLoading = false
    }
}
