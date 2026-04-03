import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var authService: AuthService
    @State private var linked: [LinkedAccount] = []
    @State private var available: [AvailableProvider] = []
    @State private var email: String?
    @State private var isLoading = true
    @State private var toastMessage: String?

    var body: some View {
        List {
            Section("Linked Accounts") {
                if isLoading {
                    ProgressView()
                } else {
                    ForEach(linked) { account in
                        HStack {
                            providerIcon(account.id)
                                .frame(width: 20, height: 20)
                            Text(account.name)
                                .font(.system(size: 15, weight: .medium))
                            if let detail = account.login ?? account.email {
                                Text(detail)
                                    .font(.system(size: 13))
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            Text("Linked")
                                .font(.system(size: 12, weight: .medium, design: .monospaced))
                                .foregroundStyle(.secondary)
                        }
                    }

                    ForEach(available) { provider in
                        Button {
                            authService.linkProvider(url: provider.url)
                        } label: {
                            HStack {
                                providerIcon(provider.id)
                                    .frame(width: 20, height: 20)
                                Text("Link \(provider.name)")
                                    .font(.system(size: 15, weight: .medium))
                                Spacer()
                                Image(systemName: "plus.circle")
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }

                    if linked.isEmpty && available.isEmpty {
                        Text("No identity providers configured")
                            .foregroundStyle(.secondary)
                    }
                }
            }

            if let email {
                Section("Account") {
                    HStack {
                        Text("Email")
                        Spacer()
                        Text(email)
                            .foregroundStyle(.secondary)
                    }
                }
            }

            Section {
                Button("Sign Out", role: .destructive) {
                    authService.logout()
                }
            }
        }
        .navigationTitle("Settings")
        .overlay {
            if let toastMessage {
                VStack {
                    Text(toastMessage)
                        .font(.system(size: 13, weight: .medium))
                        .padding(.horizontal, 16)
                        .padding(.vertical, 10)
                        .background(.ultraThinMaterial)
                        .clipShape(Capsule())
                    Spacer()
                }
                .padding(.top, 8)
                .transition(.move(edge: .top).combined(with: .opacity))
            }
        }
        .task { await loadLinked() }
        .onAppear {
            authService.onProviderLinked = { [self] in
                toastMessage = "Account linked"
                Task {
                    await loadLinked()
                    try? await Task.sleep(nanoseconds: 2_000_000_000)
                    toastMessage = nil
                }
            }
        }
        .preferredColorScheme(.dark)
    }

    private func loadLinked() async {
        guard let jwt = authService.validJWT() else { return }
        do {
            let result = try await APIClient.fetchLinkedAccounts(jwt: jwt)
            linked = result.linked
            available = result.available
            email = result.email
        } catch {}
        isLoading = false
    }

    @ViewBuilder
    private func providerIcon(_ id: String) -> some View {
        switch id {
        case "github":
            Image(systemName: "circle.fill")
                .resizable()
                .foregroundStyle(.white)
        case "google":
            Image(systemName: "g.circle.fill")
                .resizable()
                .foregroundStyle(.blue)
        default:
            Image(systemName: "link.circle")
                .resizable()
        }
    }
}
