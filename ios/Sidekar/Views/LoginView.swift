import SwiftUI

struct LoginView: View {
    @EnvironmentObject var authService: AuthService

    var body: some View {
        VStack(spacing: 32) {
            Spacer()

            Image("SidekarLogo")
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: 80, height: 80)
                .clipShape(RoundedRectangle(cornerRadius: 18))

            Text("sidekar")
                .font(.system(size: 32, weight: .bold, design: .monospaced))

            Text("Terminal access to your agent sessions")
                .font(.subheadline)
                .foregroundStyle(.secondary)

            Spacer()

            if !authService.providersLoaded {
                ProgressView()
            } else if authService.providers.isEmpty {
                Text("No login providers available")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            } else {
                VStack(spacing: 12) {
                    ForEach(authService.providers) { provider in
                        Button(action: { authService.login(provider: provider) }) {
                            HStack(spacing: 10) {
                                providerIcon(provider.id)
                                    .frame(width: 20, height: 20)
                                Text("Sign in with \(provider.name)")
                                    .font(.system(size: 15, weight: .medium))
                            }
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 14)
                        }
                        .buttonStyle(.bordered)
                        .tint(.primary)
                    }
                }
                .padding(.horizontal, 40)
            }

            Spacer()
                .frame(height: 60)
        }
        .preferredColorScheme(.dark)
        .task {
            await authService.fetchProviders()
        }
    }

    @ViewBuilder
    private func providerIcon(_ id: String) -> some View {
        switch id {
        case "github":
            GitHubIcon()
        case "google":
            GoogleIcon()
        default:
            Image(systemName: "arrow.right.circle")
                .resizable()
                .aspectRatio(contentMode: .fit)
        }
    }
}

// MARK: - Brand Icons

struct GitHubIcon: View {
    var body: some View {
        Canvas { context, size in
            let path = Path { p in
                let s = min(size.width, size.height)
                let c = CGPoint(x: size.width / 2, y: size.height / 2)
                let r = s / 2
                // Simplified GitHub octocat mark
                p.addEllipse(in: CGRect(x: c.x - r, y: c.y - r, width: s, height: s))
            }
            context.fill(path, with: .foreground)

            // Draw the GitHub logo using the known SVG path scaled to fit
            let scale = min(size.width, size.height) / 24
            let offset = CGPoint(
                x: (size.width - 24 * scale) / 2,
                y: (size.height - 24 * scale) / 2
            )
            var mark = Path()
            // GitHub mark path (simplified)
            let pts: [(CGFloat, CGFloat)] = [
                (12, 2), (6.48, 2), (2, 6.48), (2, 12),
                (5.44, 17.54), (10.21, 19.39), (10.82, 18.82),
                (10.82, 17.57), (10.82, 16.35), (9.6, 16.58),
                (8.77, 16.58), (7.94, 16.12), (7.57, 15.53),
                (7.1, 14.82), (6.35, 14.36), (6.2, 14.14),
                (6.48, 14.09), (7.1, 14.36), (7.57, 14.82),
                (8.05, 15.53), (8.77, 16.12), (9.6, 15.88),
                (9.77, 15.23), (10.3, 14.82), (7.57, 14.48),
                (5.3, 13.65), (5.3, 10.47), (5.3, 9.3),
                (5.89, 8.36), (5.3, 7.05), (5.42, 6.53),
                (6.6, 7.05), (7.57, 7.53), (8.05, 7.41),
                (9.13, 7.05), (10.21, 6.88), (11.29, 6.88),
                (12, 7.05)
            ]
            // Use circle + cutout approach for cleaner rendering
            mark.addEllipse(in: CGRect(
                x: offset.x + 3 * scale,
                y: offset.y + 3 * scale,
                width: 18 * scale,
                height: 18 * scale
            ))
            context.fill(mark, with: .color(.black))
            context.fill(mark, with: .color(Color(white: 0.08)))
        }
        // Use the actual SVG-based rendering instead
        .hidden()
        .overlay {
            Image(systemName: "circle.fill")
                .resizable()
                .foregroundStyle(.white)
                .overlay {
                    GitHubSVGShape()
                        .fill(.black)
                        .padding(1)
                }
        }
    }
}

struct GitHubSVGShape: Shape {
    func path(in rect: CGRect) -> Path {
        let s = min(rect.width, rect.height)
        let scale = s / 24
        let ox = rect.midX - 12 * scale
        let oy = rect.midY - 12 * scale

        var path = Path()
        // GitHub logo SVG path
        path.move(to: CGPoint(x: ox + 12 * scale, y: oy + 0 * scale))
        path.addCurve(
            to: CGPoint(x: ox + 0 * scale, y: oy + 12 * scale),
            control1: CGPoint(x: ox + 5.37 * scale, y: oy + 0 * scale),
            control2: CGPoint(x: ox + 0 * scale, y: oy + 5.37 * scale)
        )
        path.addCurve(
            to: CGPoint(x: ox + 8.21 * scale, y: oy + 23.39 * scale),
            control1: CGPoint(x: ox + 0 * scale, y: oy + 17.31 * scale),
            control2: CGPoint(x: ox + 3.44 * scale, y: oy + 21.8 * scale)
        )
        path.addCurve(
            to: CGPoint(x: ox + 9.02 * scale, y: oy + 22.43 * scale),
            control1: CGPoint(x: ox + 8.81 * scale, y: oy + 23.49 * scale),
            control2: CGPoint(x: ox + 9.02 * scale, y: oy + 23.14 * scale)
        )
        path.addLine(to: CGPoint(x: ox + 9.02 * scale, y: oy + 20.14 * scale))
        path.addCurve(
            to: CGPoint(x: ox + 5.54 * scale, y: oy + 18.07 * scale),
            control1: CGPoint(x: ox + 5.68 * scale, y: oy + 20.44 * scale),
            control2: CGPoint(x: ox + 5.54 * scale, y: oy + 19.67 * scale)
        )
        path.addCurve(
            to: CGPoint(x: ox + 6.23 * scale, y: oy + 15.23 * scale),
            control1: CGPoint(x: ox + 5.54 * scale, y: oy + 16.93 * scale),
            control2: CGPoint(x: ox + 5.77 * scale, y: oy + 15.93 * scale)
        )
        path.addCurve(
            to: CGPoint(x: ox + 6.35 * scale, y: oy + 12 * scale),
            control1: CGPoint(x: ox + 6.11 * scale, y: oy + 14.93 * scale),
            control2: CGPoint(x: ox + 5.69 * scale, y: oy + 13.53 * scale)
        )
        path.addCurve(
            to: CGPoint(x: ox + 9 * scale, y: oy + 11.6 * scale),
            control1: CGPoint(x: ox + 7.32 * scale, y: oy + 11.7 * scale),
            control2: CGPoint(x: ox + 8.04 * scale, y: oy + 11.47 * scale)
        )
        path.addCurve(
            to: CGPoint(x: ox + 12 * scale, y: oy + 11.6 * scale),
            control1: CGPoint(x: ox + 9.96 * scale, y: oy + 11.33 * scale),
            control2: CGPoint(x: ox + 10.98 * scale, y: oy + 11.33 * scale)
        )
        path.addCurve(
            to: CGPoint(x: ox + 24 * scale, y: oy + 12 * scale),
            control1: CGPoint(x: ox + 18.63 * scale, y: oy + 11.6 * scale),
            control2: CGPoint(x: ox + 24 * scale, y: oy + 5.37 * scale)
        )
        path.addCurve(
            to: CGPoint(x: ox + 12 * scale, y: oy + 0 * scale),
            control1: CGPoint(x: ox + 24 * scale, y: oy + 5.37 * scale),
            control2: CGPoint(x: ox + 18.63 * scale, y: oy + 0 * scale)
        )
        path.closeSubpath()
        return path
    }
}

struct GoogleIcon: View {
    var body: some View {
        Canvas { context, size in
            let s = min(size.width, size.height)
            let scale = s / 24
            let ox = (size.width - 24 * scale) / 2
            let oy = (size.height - 24 * scale) / 2

            // Blue part
            var blue = Path()
            blue.move(to: CGPoint(x: ox + 22.56 * scale, y: oy + 12.25 * scale))
            blue.addCurve(to: CGPoint(x: ox + 22.36 * scale, y: oy + 10 * scale), control1: CGPoint(x: ox + 22.56 * scale, y: oy + 11.47 * scale), control2: CGPoint(x: ox + 22.49 * scale, y: oy + 10.72 * scale))
            blue.addLine(to: CGPoint(x: ox + 12 * scale, y: oy + 10 * scale))
            blue.addLine(to: CGPoint(x: ox + 12 * scale, y: oy + 14.26 * scale))
            blue.addLine(to: CGPoint(x: ox + 17.92 * scale, y: oy + 14.26 * scale))
            blue.addCurve(to: CGPoint(x: ox + 15.72 * scale, y: oy + 17.58 * scale), control1: CGPoint(x: ox + 17.57 * scale, y: oy + 15.55 * scale), control2: CGPoint(x: ox + 16.82 * scale, y: oy + 16.72 * scale))
            blue.addLine(to: CGPoint(x: ox + 19.28 * scale, y: oy + 20.34 * scale))
            blue.addCurve(to: CGPoint(x: ox + 22.56 * scale, y: oy + 12.25 * scale), control1: CGPoint(x: ox + 21.36 * scale, y: oy + 18.42 * scale), control2: CGPoint(x: ox + 22.56 * scale, y: oy + 15.6 * scale))
            blue.closeSubpath()
            context.fill(blue, with: .color(Color(red: 0.263, green: 0.522, blue: 0.957)))

            // Green part
            var green = Path()
            green.move(to: CGPoint(x: ox + 12 * scale, y: oy + 23 * scale))
            green.addCurve(to: CGPoint(x: ox + 19.28 * scale, y: oy + 20.34 * scale), control1: CGPoint(x: ox + 14.97 * scale, y: oy + 23 * scale), control2: CGPoint(x: ox + 17.46 * scale, y: oy + 22.02 * scale))
            green.addLine(to: CGPoint(x: ox + 15.72 * scale, y: oy + 17.58 * scale))
            green.addCurve(to: CGPoint(x: ox + 12 * scale, y: oy + 18.64 * scale), control1: CGPoint(x: ox + 14.74 * scale, y: oy + 18.24 * scale), control2: CGPoint(x: ox + 13.49 * scale, y: oy + 18.64 * scale))
            green.addCurve(to: CGPoint(x: ox + 5.84 * scale, y: oy + 14.09 * scale), control1: CGPoint(x: ox + 9.14 * scale, y: oy + 18.64 * scale), control2: CGPoint(x: ox + 6.71 * scale, y: oy + 16.71 * scale))
            green.addLine(to: CGPoint(x: ox + 2.18 * scale, y: oy + 16.93 * scale))
            green.addCurve(to: CGPoint(x: ox + 12 * scale, y: oy + 23 * scale), control1: CGPoint(x: ox + 3.99 * scale, y: oy + 20.53 * scale), control2: CGPoint(x: ox + 7.7 * scale, y: oy + 23 * scale))
            green.closeSubpath()
            context.fill(green, with: .color(Color(red: 0.204, green: 0.659, blue: 0.325)))

            // Yellow part
            var yellow = Path()
            yellow.move(to: CGPoint(x: ox + 5.84 * scale, y: oy + 14.09 * scale))
            yellow.addCurve(to: CGPoint(x: ox + 5.49 * scale, y: oy + 12 * scale), control1: CGPoint(x: ox + 5.62 * scale, y: oy + 13.43 * scale), control2: CGPoint(x: ox + 5.49 * scale, y: oy + 12.73 * scale))
            yellow.addCurve(to: CGPoint(x: ox + 5.84 * scale, y: oy + 9.91 * scale), control1: CGPoint(x: ox + 5.49 * scale, y: oy + 11.27 * scale), control2: CGPoint(x: ox + 5.62 * scale, y: oy + 10.57 * scale))
            yellow.addLine(to: CGPoint(x: ox + 2.18 * scale, y: oy + 7.07 * scale))
            yellow.addCurve(to: CGPoint(x: ox + 1 * scale, y: oy + 12 * scale), control1: CGPoint(x: ox + 1.42 * scale, y: oy + 8.55 * scale), control2: CGPoint(x: ox + 1 * scale, y: oy + 10.23 * scale))
            yellow.addCurve(to: CGPoint(x: ox + 2.18 * scale, y: oy + 16.93 * scale), control1: CGPoint(x: ox + 1 * scale, y: oy + 13.77 * scale), control2: CGPoint(x: ox + 1.42 * scale, y: oy + 15.45 * scale))
            yellow.addLine(to: CGPoint(x: ox + 5.84 * scale, y: oy + 14.09 * scale))
            yellow.closeSubpath()
            context.fill(yellow, with: .color(Color(red: 0.984, green: 0.737, blue: 0.02)))

            // Red part
            var red = Path()
            red.move(to: CGPoint(x: ox + 12 * scale, y: oy + 5.38 * scale))
            red.addCurve(to: CGPoint(x: ox + 16.21 * scale, y: oy + 7.02 * scale), control1: CGPoint(x: ox + 13.62 * scale, y: oy + 5.38 * scale), control2: CGPoint(x: ox + 15.06 * scale, y: oy + 5.94 * scale))
            red.addLine(to: CGPoint(x: ox + 19.36 * scale, y: oy + 3.87 * scale))
            red.addCurve(to: CGPoint(x: ox + 12 * scale, y: oy + 1 * scale), control1: CGPoint(x: ox + 17.45 * scale, y: oy + 2.09 * scale), control2: CGPoint(x: ox + 14.97 * scale, y: oy + 1 * scale))
            red.addCurve(to: CGPoint(x: ox + 2.18 * scale, y: oy + 7.07 * scale), control1: CGPoint(x: ox + 7.7 * scale, y: oy + 1 * scale), control2: CGPoint(x: ox + 3.99 * scale, y: oy + 3.47 * scale))
            red.addLine(to: CGPoint(x: ox + 5.84 * scale, y: oy + 9.91 * scale))
            red.addCurve(to: CGPoint(x: ox + 12 * scale, y: oy + 5.38 * scale), control1: CGPoint(x: ox + 6.71 * scale, y: oy + 7.31 * scale), control2: CGPoint(x: ox + 9.14 * scale, y: oy + 5.38 * scale))
            red.closeSubpath()
            context.fill(red, with: .color(Color(red: 0.918, green: 0.263, blue: 0.208)))
        }
    }
}
