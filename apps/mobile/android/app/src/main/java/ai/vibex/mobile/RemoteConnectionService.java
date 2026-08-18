package ai.vibex.mobile;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.os.IBinder;

/** Keeps the authenticated desktop connection runnable while the activity is backgrounded. */
public final class RemoteConnectionService extends Service {
    private static final String CHANNEL_ID = "remote_connection";
    private static final int NOTIFICATION_ID = 0x564258;

    public static void start(Context context) {
        Context applicationContext = context.getApplicationContext();
        applicationContext.startForegroundService(
                new Intent(applicationContext, RemoteConnectionService.class));
    }

    public static void stop(Context context) {
        Context applicationContext = context.getApplicationContext();
        applicationContext.stopService(
                new Intent(applicationContext, RemoteConnectionService.class));
    }

    @Override
    public void onCreate() {
        super.onCreate();
        NotificationManager manager = getSystemService(NotificationManager.class);
        NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID,
                getString(R.string.remote_connection_channel_name),
                NotificationManager.IMPORTANCE_LOW);
        channel.setDescription(getString(R.string.remote_connection_channel_description));
        channel.setShowBadge(false);
        manager.createNotificationChannel(channel);
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        Intent openApp = new Intent(this, GpuiNativeActivity.class)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        PendingIntent contentIntent = PendingIntent.getActivity(
                this,
                0,
                openApp,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        Notification notification = new Notification.Builder(this, CHANNEL_ID)
                .setSmallIcon(R.drawable.ic_launcher_foreground)
                .setContentTitle(getString(R.string.remote_connection_notification_title))
                .setContentText(getString(R.string.remote_connection_notification_body))
                .setCategory(Notification.CATEGORY_SERVICE)
                .setContentIntent(contentIntent)
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
