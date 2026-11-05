package com.example.qprofprobe;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.os.IBinder;

public class StandService extends Service {
    private static final String CHANNEL = "qstream-stand";

    @Override
    public void onCreate() {
        super.onCreate();
        NotificationManager nm = getSystemService(NotificationManager.class);
        NotificationChannel ch = new NotificationChannel(
                CHANNEL, "qstream stand", NotificationManager.IMPORTANCE_LOW);
        nm.createNotificationChannel(ch);

        Notification n = new Notification.Builder(this, CHANNEL)
                .setContentTitle("qstream stand")
                .setContentText("layer host is running")
                .setSmallIcon(android.R.drawable.stat_sys_warning)
                .build();
        startForeground(1, n);
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        return START_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
