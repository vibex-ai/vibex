package com.longbridge.nsh;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.os.IBinder;

/**
 * Keeps the process — and the authenticated desktop connection — runnable
 * while the activity is in the background.
 *
 * <p>The Rust app starts it once a runtime is connected and stops it when the
 * connection ends; the notification is the system's statement that the app is
 * still working, and its tap returns to the session.
 */
public final class ConnectionService extends Service {
    private static final String CHANNEL_ID = "vibex_connection";
    private static final int NOTIFICATION_ID = 0x564258;

    public static void start(Context context) {
        Context app = context.getApplicationContext();
        app.startForegroundService(new Intent(app, ConnectionService.class));
    }

    public static void stop(Context context) {
        Context app = context.getApplicationContext();
        app.stopService(new Intent(app, ConnectionService.class));
    }

    @Override
    public void onCreate() {
        super.onCreate();
        NotificationManager manager = getSystemService(NotificationManager.class);
        NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID, "Vibex connection", NotificationManager.IMPORTANCE_LOW);
        channel.setDescription("Keeps the desktop connection alive in the background");
        channel.setShowBadge(false);
        manager.createNotificationChannel(channel);
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        Intent open = new Intent(this, MainActivity.class)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        PendingIntent content = PendingIntent.getActivity(
                this, 0, open, PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        Notification notification = new Notification.Builder(this, CHANNEL_ID)
                .setSmallIcon(android.R.drawable.stat_notify_sync)
                .setContentTitle("Vibex")
                .setContentText("Connected to your desktop")
                .setCategory(Notification.CATEGORY_SERVICE)
                .setContentIntent(content)
                .setOnlyAlertOnce(true)
                .setOngoing(true)
                .setShowWhen(false)
                .build();
        startForeground(NOTIFICATION_ID, notification);
        return START_NOT_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
