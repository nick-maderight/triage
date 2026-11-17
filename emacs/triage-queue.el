;;; triage-queue.el --- Shared queue state, stream and actions for triage -*- lexical-binding: t; -*-

;;; Commentary:

;; This file holds the queue that every triage view reads.  It refreshes the
;; queue from the daemon, keeps the SSE stream open with backoff, and runs card
;; actions optimistically: the card leaves the queue at once and returns with
;; its draft if the daemon refuses the action.

;;; Code:

(require 'cl-lib)
(require 'subr-x)
(require 'triage-api)
(require 'triage-card)

(defvar triage-queue-cards nil
  "Queue cards sorted by `triage-card-less-p', minus cards in flight.")

(defvar triage-queue-counts nil
  "Counts of the queue cards by kind, from `triage-card-counts'.")

(defvar triage-queue-version nil
  "Queue version the daemon reported on the last good refresh.")

(defvar triage-queue-connection 'connecting
  "Connection state: `connecting', `connected' or `reconnecting'.")

(defvar triage-queue-initialized nil
  "Non-nil after the first good queue refresh.")

(defvar triage-queue-unreachable-since nil
  "Float time when the daemon became unreachable, or nil when reachable.")

(defvar triage-queue-update-hook nil
  "Hook run after every queue apply, local action and connection change.")

(defvar triage-queue--inflight (make-hash-table :test #'equal)
  "Hash of card id to t for actions the daemon has not answered yet.")

(defvar triage-queue--drafts (make-hash-table :test #'equal)
  "Hash of card id to the `triage-draft' typed into that card.")

(defvar triage-queue--refreshing nil
  "Non-nil while a queue request is in flight.")

(defvar triage-queue--dirty nil
  "Non-nil when a refresh was requested while one was in flight.")

(defvar triage-queue--stream nil
  "The SSE stream process, or nil when no stream is open.")

(defvar triage-queue--retry-delay 1
  "Seconds before the next reconnect attempt, doubled after each failure.")

(defvar triage-queue--retry-timer nil
  "Timer for the next reconnect attempt, or nil.")

(defvar triage-queue--consumers nil
  "Symbols of the views that need the queue while they are open.")

(cl-defstruct (triage-draft (:constructor triage-draft--make))
  "Text and choices typed into one card before it is sent."
  (note "") (other "") (step 0) (answers nil) (executor "") (executor-thinking ""))

(defun triage-queue-draft (card)
  "Return the draft for CARD, creating an empty one on first use."
  (let ((id (alist-get 'id card)))
    (or (gethash id triage-queue--drafts)
        (puthash id (triage-draft--make) triage-queue--drafts))))

(defun triage-queue-drop-draft (id)
  "Forget the draft for the card with ID."
  (remhash id triage-queue--drafts))

(defun triage-queue-find (id)
  "Return the queue card whose id is ID, or nil."
  (cl-find id triage-queue-cards
           :key (lambda (card) (alist-get 'id card)) :test #'equal))

(defun triage-queue--cancel-retry ()
  "Cancel the pending reconnect timer, if any."
  (when (timerp triage-queue--retry-timer)
    (cancel-timer triage-queue--retry-timer))
  (setq triage-queue--retry-timer nil))

(defun triage-queue--schedule-retry ()
  "Arm the reconnect timer and double the delay, capped at 30 seconds."
  (triage-queue--cancel-retry)
  (setq triage-queue--retry-timer
        (run-at-time triage-queue--retry-delay nil #'triage-queue--reconnect))
  (setq triage-queue--retry-delay (min 30 (* 2 triage-queue--retry-delay))))

(defun triage-queue--stream-live-p ()
  "Return non-nil when the SSE stream process is running."
  (and (processp triage-queue--stream)
       (process-live-p triage-queue--stream)))

(defun triage-queue--reconnect ()
  "Restore the stream, or refresh when it is open, while anyone is subscribed."
  (setq triage-queue--retry-timer nil)
  (when triage-queue--consumers
    (if (triage-queue--stream-live-p)
        (triage-queue-refresh)
      ;; Starting the stream refreshes the queue, so no second refresh is needed.
      (triage-queue--start-stream))))

(defun triage-queue--start-stream ()
  "Open the SSE stream, cancel any pending retry, and refresh the queue."
  (triage-queue--cancel-retry)
  (setq triage-queue--stream
        (triage-api-stream (lambda (_data) (triage-queue-refresh))
                           #'triage-queue--stream-closed))
  (triage-queue-refresh))

(defun triage-queue--stream-closed ()
  "Clear the stream and schedule a reconnect while anyone is subscribed."
  (setq triage-queue--stream nil)
  (when triage-queue--consumers
    (setq triage-queue-connection 'reconnecting)
    (unless triage-queue-unreachable-since
      (setq triage-queue-unreachable-since (float-time)))
    (run-hooks 'triage-queue-update-hook)
    (triage-queue--schedule-retry)))

(defun triage-queue-subscribe (who)
  "Add WHO as a consumer and open the stream when none is live."
  (cl-pushnew who triage-queue--consumers)
  (unless (triage-queue--stream-live-p)
    (triage-queue--start-stream)))

(defun triage-queue-unsubscribe (who)
  "Remove WHO as a consumer and close the stream when none remain."
  (setq triage-queue--consumers (delq who triage-queue--consumers))
  (unless triage-queue--consumers
    (when (processp triage-queue--stream)
      (delete-process triage-queue--stream))
    (setq triage-queue--stream nil)
    (triage-queue--cancel-retry)
    (setq triage-queue--retry-delay 1
          triage-queue-unreachable-since nil
          triage-queue-connection 'connecting)
    (run-hooks 'triage-queue-update-hook)))

(defun triage-queue--queue-body-p (body)
  "Return non-nil when BODY is a queue response with a list of cards."
  (and (listp body)
       (assq 'cards body)
       (listp (alist-get 'cards body))))

(defun triage-queue--apply (body)
  "Replace the queue with the good queue response BODY and prune drafts."
  (let ((incoming (alist-get 'cards body))
        (known (make-hash-table :test #'equal))
        stale)
    (dolist (card incoming)
      (puthash (alist-get 'id card) t known))
    (maphash (lambda (id _) (puthash id t known)) triage-queue--inflight)
    (maphash (lambda (id _) (unless (gethash id known) (push id stale)))
             triage-queue--drafts)
    (dolist (id stale)
      (remhash id triage-queue--drafts))
    (setq triage-queue-cards
          (cl-remove-if (lambda (card)
                          (gethash (alist-get 'id card) triage-queue--inflight))
                        (triage-card-sort incoming)))
    (setq triage-queue-counts (triage-card-counts triage-queue-cards)
          triage-queue-version (alist-get 'version body)
          triage-queue-connection 'connected
          triage-queue-initialized t
          triage-queue-unreachable-since nil
          triage-queue--retry-delay 1)
    (triage-queue--cancel-retry)
    (run-hooks 'triage-queue-update-hook)))

(defun triage-queue--refresh-failed (reason)
  "Record a failed queue refresh for REASON and schedule a retry."
  (when (eq triage-queue-connection 'connected)
    (message "Queue refresh failed: %s" reason))
  (setq triage-queue-connection 'reconnecting)
  (unless triage-queue-unreachable-since
    (setq triage-queue-unreachable-since (float-time)))
  (run-hooks 'triage-queue-update-hook)
  (triage-queue--schedule-retry))

(defun triage-queue--finish-refresh ()
  "Clear the refreshing flag and run again if a refresh was requested meanwhile."
  (setq triage-queue--refreshing nil)
  (when triage-queue--dirty
    (setq triage-queue--dirty nil)
    (triage-queue-refresh)))

(defun triage-queue-refresh ()
  "Fetch the queue from the daemon and apply it.
A refresh requested while one is in flight runs again once it finishes."
  (if triage-queue--refreshing
      (setq triage-queue--dirty t)
    (setq triage-queue--refreshing t)
    (triage-api-request
     'get "/v1/queue"
     :then (lambda (body)
             (unwind-protect
                 (if (triage-queue--queue-body-p body)
                     (triage-queue--apply body)
                   (triage-queue--refresh-failed "Unexpected queue response"))
               (triage-queue--finish-refresh)))
     :else (lambda (_status reason)
             (unwind-protect
                 (triage-queue--refresh-failed reason)
               (triage-queue--finish-refresh))))))

(defun triage-queue--settle (id)
  "Remove ID from the in-flight set and refresh the queue."
  (remhash id triage-queue--inflight)
  (triage-queue-refresh))

(defun triage-queue-act (card verb body &optional choice)
  "Run VERB with BODY on CARD, removing it until the daemon responds.
CHOICE is the chosen Later option for the result message.  Return nil if
CARD is absent from the queue, else t."
  (let* ((id (alist-get 'id card))
         (queued (triage-queue-find id)))
    (when queued
      (puthash id t triage-queue--inflight)
      (setq triage-queue-cards (delq queued triage-queue-cards)
            triage-queue-counts (triage-card-counts triage-queue-cards))
      (message "%s" (triage-card-result-message queued verb body choice))
      (run-hooks 'triage-queue-update-hook)
      (triage-api-request
       'post (triage-api-card-path id verb)
       :body body
       :then (lambda (_response)
               (triage-queue-drop-draft id)
               (triage-queue--settle id))
       :else (lambda (status reason)
               (if (memq status '(409 404))
                   (progn
                     (message "Already handled elsewhere · %s"
                              (triage-card-title-of queued))
                     (triage-queue-drop-draft id))
                 (message "%s" (propertize (format "! %s · %s" reason
                                                   (triage-card-title-of queued))
                                           'face 'error)))
               (triage-queue--settle id)))
      t)))

(provide 'triage-queue)
;;; triage-queue.el ends here
